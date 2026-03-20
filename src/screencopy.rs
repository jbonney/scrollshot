//! Screen capture loop: takes screenshots of the selected region, scrolls down,
//! and repeats until the content stops changing.

use crate::Rect;
use anyhow::{anyhow, Result};
use image::RgbaImage;
use std::io::{Seek, SeekFrom};
use std::os::fd::AsFd;
use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};

// ── Tuning parameters ─────────────────────────────────────────────────────────

/// Average per-channel pixel difference below which two frames are "the same".
const DIFF_THRESHOLD: f64 = 1.5;
/// Number of consecutive unchanged frames before capture stops.
const STOP_STREAK: usize = 2;
/// Maximum frames to capture regardless of content changes.
const MAX_FRAMES: usize = 200;
/// Maximum attempts to wait for the first frame to stabilize (e.g. lazy images).
const STABILIZE_ATTEMPTS: usize = 10;

// ── Capture state ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct FrameInfo {
    format: u32,
    width: u32,
    height: u32,
    stride: u32,
    ready: bool,
    failed: bool,
}

struct CaptureState {
    shm: Option<wl_shm::WlShm>,
    output: Option<wl_output::WlOutput>,
    screencopy_manager: Option<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1>,
    vp_manager: Option<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>,
    seat: Option<wl_seat::WlSeat>,

    // Per-capture session
    current_frame_id: u32,    // incremented for each capture; events from old IDs are ignored
    frame_info: FrameInfo,
    capture_buf_file: Option<std::fs::File>,
    capture_buf: Option<wl_buffer::WlBuffer>,

    // Output size (for virtual pointer motion_absolute)
    output_w: u32,
    output_h: u32,

    // Registry global name of the output to capture (0 = first available).
    output_global_name: u32,
}

impl CaptureState {
    fn new() -> Self {
        CaptureState {
            shm: None,
            output: None,
            screencopy_manager: None,
            vp_manager: None,
            seat: None,
            current_frame_id: 0,
            frame_info: FrameInfo::default(),
            capture_buf_file: None,
            capture_buf: None,
            output_w: 0,
            output_h: 0,
            output_global_name: 0,
        }
    }
}

// ── Dispatch impls ────────────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for CaptureState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "wl_output" => {
                    // Bind only the output the selector was displayed on.
                    // global name 0 means "first available" (fallback).
                    if state.output.is_none()
                        && (state.output_global_name == 0 || name == state.output_global_name)
                    {
                        state.output = Some(registry.bind(name, version.min(3), qh, ()));
                    }
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()));
                }
                "zwlr_screencopy_manager_v1" => {
                    state.screencopy_manager = Some(registry.bind(name, version.min(3), qh, ()));
                }
                "zwlr_virtual_pointer_manager_v1" => {
                    state.vp_manager = Some(registry.bind(name, version.min(2), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Mode { width, height, .. } = event {
            if width > 0 && height > 0 {
                state.output_w = width as u32;
                state.output_h = height as u32;
            }
        }
    }
}

impl Dispatch<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1, ()> for CaptureState {
    fn event(
        _: &mut Self,
        _: &zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
        _: zwlr_screencopy_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1, u32> for CaptureState {
    fn event(
        state: &mut Self,
        frame: &zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        frame_id: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // Ignore events from previous (stale) frames
        if *frame_id != state.current_frame_id {
            return;
        }

        match event {
            zwlr_screencopy_frame_v1::Event::Buffer { format, width, height, stride } => {
                // Skip if we already set up the buffer (v3 sends multiple Buffer events)
                if state.capture_buf.is_some() {
                    return;
                }

                let fmt = match format {
                    WEnum::Value(f) => f,
                    WEnum::Unknown(_) => wl_shm::Format::Xrgb8888,
                };
                let fmt_u32 = fmt as u32;

                state.frame_info.format = fmt_u32;
                state.frame_info.width = width;
                state.frame_info.height = height;
                state.frame_info.stride = stride;

                // Create the SHM buffer and call copy immediately.
                // This works for all protocol versions (v1 doesn't send buffer_done).
                let shm = match state.shm.as_ref() {
                    Some(s) => s,
                    None => { state.frame_info.failed = true; return; }
                };

                let buf_size = (stride * height) as usize;
                let file = match tempfile::tempfile() {
                    Ok(f) => f,
                    Err(_) => { state.frame_info.failed = true; return; }
                };
                if file.set_len(buf_size as u64).is_err() {
                    state.frame_info.failed = true;
                    return;
                }

                let pool = shm.create_pool(file.as_fd(), buf_size as i32, qh, ());
                let buffer = pool.create_buffer(
                    0,
                    width as i32,
                    height as i32,
                    stride as i32,
                    fmt,
                    qh,
                    (),
                );
                pool.destroy();

                frame.copy(&buffer);

                state.capture_buf_file = Some(file);
                state.capture_buf = Some(buffer);
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => {}
            zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                state.frame_info.ready = true;
            }
            zwlr_screencopy_frame_v1::Event::Failed => {
                state.frame_info.failed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, ()> for CaptureState {
    fn event(
        _: &mut Self,
        _: &zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        _: zwlr_virtual_pointer_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, ()> for CaptureState {
    fn event(
        _: &mut Self,
        _: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
        _: zwlr_virtual_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for CaptureState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(CaptureState: ignore wl_shm::WlShm);
delegate_noop!(CaptureState: ignore wl_shm_pool::WlShmPool);
delegate_noop!(CaptureState: ignore wl_buffer::WlBuffer);

// ── Frame conversion ──────────────────────────────────────────────────────────

/// Convert the raw SHM bytes to an `RgbaImage`.
/// Handles the two most common formats from wlr-screencopy:
///   XRGB8888 / ARGB8888  → memory layout [B, G, R, X/A]
///   XBGR8888 / ABGR8888  → memory layout [R, G, B, X/A]
fn raw_to_rgba(data: &[u8], width: u32, height: u32, stride: u32, format: u32) -> RgbaImage {
    let mut img = RgbaImage::new(width, height);

    // wl_shm format values (same as DRM fourcc):
    // ARGB8888 = 0, XRGB8888 = 1, ABGR8888 = 0x34324241, XBGR8888 = 0x34324258
    let bgr = matches!(format, 0 | 1); // ARGB / XRGB → bytes in mem: [B,G,R,A]

    for y in 0..height {
        for x in 0..width {
            let off = (y * stride + x * 4) as usize;
            if off + 4 > data.len() {
                break;
            }
            let (r, g, b, a) = if bgr {
                (data[off + 2], data[off + 1], data[off], data[off + 3])
            } else {
                // XBGR / ABGR → bytes [R,G,B,A]
                (data[off], data[off + 1], data[off + 2], data[off + 3])
            };
            img.put_pixel(x, y, image::Rgba([r, g, b, if format == 0 || format == 0x34324241 { a } else { 255 }]));
        }
    }
    img
}

// ── Capture helpers ───────────────────────────────────────────────────────────

/// Capture one frame of the given output region and return it as RgbaImage.
fn capture_one(
    state: &mut CaptureState,
    queue: &mut wayland_client::EventQueue<CaptureState>,
    region: Rect,
) -> Result<RgbaImage> {
    let manager = state
        .screencopy_manager
        .as_ref()
        .ok_or_else(|| anyhow!("zwlr_screencopy_manager_v1 not available"))?;
    let output = state
        .output
        .as_ref()
        .ok_or_else(|| anyhow!("wl_output not available"))?;
    let qh = queue.handle();

    state.current_frame_id = state.current_frame_id.wrapping_add(1);
    let frame_id = state.current_frame_id;
    state.frame_info = FrameInfo::default();
    state.capture_buf_file = None;
    state.capture_buf = None;

    let _frame = manager.capture_output_region(
        0, // overlay_cursor = false
        output,
        region.x,
        region.y,
        region.width as i32,
        region.height as i32,
        &qh,
        frame_id,
    );

    // Dispatch until the frame is ready or failed
    loop {
        queue.blocking_dispatch(state)?;
        if state.frame_info.ready || state.frame_info.failed {
            break;
        }
    }

    if state.frame_info.failed {
        return Err(anyhow!("screencopy frame failed"));
    }

    // Read the SHM data
    let file = state
        .capture_buf_file
        .as_mut()
        .ok_or_else(|| anyhow!("no capture buffer file"))?;
    file.seek(SeekFrom::Start(0))?;
    let mut raw = Vec::with_capacity((state.frame_info.stride * state.frame_info.height) as usize);
    use std::io::Read;
    file.read_to_end(&mut raw)?;

    Ok(raw_to_rgba(
        &raw,
        state.frame_info.width,
        state.frame_info.height,
        state.frame_info.stride,
        state.frame_info.format,
    ))
}

/// Scroll down inside the region using a virtual pointer.
fn scroll_down(
    state: &mut CaptureState,
    queue: &mut wayland_client::EventQueue<CaptureState>,
    region: Rect,
    ticks: i32,
) -> Result<()> {
    let vp_manager = state
        .vp_manager
        .as_ref()
        .ok_or_else(|| anyhow!("zwlr_virtual_pointer_manager_v1 not available"))?;
    let seat = state.seat.as_ref();
    let qh = queue.handle();

    let vp = vp_manager.create_virtual_pointer(seat, &qh, ());

    let cx = (region.x + region.width as i32 / 2) as u32;
    let cy = (region.y + region.height as i32 / 2) as u32;
    let ow = state.output_w;
    let oh = state.output_h;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32;

    // Position pointer at the center of the region
    vp.motion_absolute(ts, cx, cy, ow, oh);
    vp.frame();

    // Send discrete scroll ticks
    for i in 0..ticks {
        let t = ts + i as u32;
        vp.axis_source(wl_pointer::AxisSource::Wheel);
        // positive value = scroll down (natural direction)
        vp.axis_discrete(t, wl_pointer::Axis::VerticalScroll, 15.0, 1);
        vp.frame();
    }

    vp.destroy();
    queue.flush()?;
    Ok(())
}

/// Compare two frames; returns average per-channel pixel difference (0..255).
fn frame_diff(a: &RgbaImage, b: &RgbaImage) -> f64 {
    if a.width() != b.width() || a.height() != b.height() {
        return 255.0;
    }
    let mut total: u64 = 0;
    let mut count: u64 = 0;
    // Sample every 4th pixel in each direction for speed
    for y in (0..a.height()).step_by(4) {
        for x in (0..a.width()).step_by(4) {
            let pa = a.get_pixel(x, y);
            let pb = b.get_pixel(x, y);
            total += (pa[0] as i32 - pb[0] as i32).unsigned_abs() as u64;
            total += (pa[1] as i32 - pb[1] as i32).unsigned_abs() as u64;
            total += (pa[2] as i32 - pb[2] as i32).unsigned_abs() as u64;
            count += 3;
        }
    }
    if count == 0 { 0.0 } else { total as f64 / count as f64 }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Capture a scrolling screenshot of `region` on the output identified by
/// `output_global_name` (the Wayland registry global name returned by
/// `selector::select_region`).  Pass 0 to fall back to the first available output.
pub fn capture_scrolling(
    region: Rect,
    output_global_name: u32,
    settle_ms: u64,
    scroll_ticks: i32,
) -> Result<Vec<RgbaImage>> {
    let conn = Connection::connect_to_env()
        .map_err(|e| anyhow!("Cannot connect to Wayland display: {e}"))?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();

    conn.display().get_registry(&qh, ());

    let mut state = CaptureState::new();
    state.output_global_name = output_global_name;
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?; // second roundtrip to get output geometry

    if state.screencopy_manager.is_none() {
        return Err(anyhow!(
            "zwlr_screencopy_manager_v1 not available — is your compositor wlroots-based?"
        ));
    }
    if state.vp_manager.is_none() {
        return Err(anyhow!(
            "zwlr_virtual_pointer_manager_v1 not available"
        ));
    }

    let mut frames: Vec<RgbaImage> = Vec::new();
    let mut no_change_streak = 0;

    // Capture first frame, then wait for it to stabilize (lazy-loaded images, etc.)
    let first = capture_one(&mut state, &mut queue, region)?;
    frames.push(first);
    for attempt in 0..STABILIZE_ATTEMPTS {
        std::thread::sleep(std::time::Duration::from_millis(settle_ms));
        let probe = capture_one(&mut state, &mut queue, region)?;
        let diff = frame_diff(frames.last().unwrap(), &probe);
        if diff < DIFF_THRESHOLD {
            if attempt > 0 {
                eprintln!("  initial frame stabilized after {} extra captures", attempt + 1);
            }
            break;
        }
        // Still loading — advance frame[0] to the most recent state
        *frames.last_mut().unwrap() = probe;
    }

    loop {
        if frames.len() >= MAX_FRAMES {
            eprintln!("Reached frame limit ({MAX_FRAMES}), stopping.");
            break;
        }

        scroll_down(&mut state, &mut queue, region, scroll_ticks)?;

        // Wait for the page to render
        std::thread::sleep(std::time::Duration::from_millis(settle_ms));

        let frame = capture_one(&mut state, &mut queue, region)?;
        let diff = frame_diff(frames.last().unwrap(), &frame);
        eprintln!("  frame {}: diff={:.2}", frames.len() + 1, diff);

        if diff < DIFF_THRESHOLD {
            no_change_streak += 1;
            if no_change_streak >= STOP_STREAK {
                eprintln!("Content stopped changing — reached bottom.");
                break;
            }
        } else {
            no_change_streak = 0;
            frames.push(frame);
        }
    }

    Ok(frames)
}
