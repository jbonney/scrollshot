//! Region selector: shows a fullscreen layer-shell overlay and lets the user
//! click-drag to choose a rectangle.  Returns `None` on ESC or right-click.

use crate::Rect;
use anyhow::{anyhow, Result, Context};
use std::os::fd::AsFd;
use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry,
        wl_seat, wl_shm, wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const KEY_ESC: u32 = 1; // evdev keycode

// SAFETY: SelectorState contains a raw `*mut u8` (overlay_mmap) which is not
// Send/Sync by default.  It is safe to assert both traits here because:
//   1. The state is created, used, and dropped entirely within `select_region`,
//      which never moves it across threads.
//   2. The Wayland event queue (`blocking_dispatch`) is driven single-threadedly
//      in that same function; no other thread ever touches the pointer.
//   3. The mmap region is unmapped in `Drop` before the state leaves scope.
// If this code is ever refactored to use threads, these impls must be revisited.
unsafe impl Send for SelectorState {}
unsafe impl Sync for SelectorState {}

// ── State ─────────────────────────────────────────────────────────────────────

struct SelectorState {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,

    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    pointer: Option<wl_pointer::WlPointer>,

    // From wl_output::mode (physical resolution, used as fallback)
    screen_w: u32,
    screen_h: u32,
    output_ready: bool,

    // Set after layer surface configure event
    canvas_w: u32,
    canvas_h: u32,

    // Overlay buffer: mmap'd shared memory written directly on each redraw
    overlay_mmap: *mut u8,   // null if not yet allocated
    overlay_mmap_size: usize,
    overlay_file: Option<std::fs::File>,
    overlay_buffer: Option<wl_buffer::WlBuffer>,
    // Previous selection bounding box (normalised), for incremental damage
    prev_sel: Option<(u32, u32, u32, u32)>, // x1,y1,x2,y2

    // Frame-callback rate-limiting: only one pending Wayland commit at a time.
    frame_pending: bool,
    frame_callback: Option<wl_callback::WlCallback>,
    pending_sel: Option<Option<(i32, i32, i32, i32)>>, // queued selection during pending frame

    // Cursor tracking for crosshair drawing
    cursor_pos: Option<(u32, u32)>,
    prev_cursor_bbox: Option<(u32, u32, u32, u32)>, // x1,y1,x2,y2 of previously drawn crosshair

    // Drag state
    pressing: bool,
    drag_start: Option<(f64, f64)>,
    drag_end: Option<(f64, f64)>,

    done: bool,
    cancelled: bool,
    init_error: Option<String>,
}

impl SelectorState {
    fn new() -> Self {
        SelectorState {
            compositor: None,
            shm: None,
            seat: None,
            layer_shell: None,
            surface: None,
            layer_surface: None,
            pointer: None,
            screen_w: 0,
            screen_h: 0,
            output_ready: false,
            canvas_w: 0,
            canvas_h: 0,
            overlay_mmap: std::ptr::null_mut(),
            overlay_mmap_size: 0,
            overlay_file: None,
            overlay_buffer: None,
            prev_sel: None,
            frame_pending: false,
            frame_callback: None,
            pending_sel: None,
            cursor_pos: None,
            prev_cursor_bbox: None,
            pressing: false,
            drag_start: None,
            drag_end: None,
            done: false,
            cancelled: false,
            init_error: None,
        }
    }

    /// Create the layer-shell surface once we have compositor + shm + layer_shell + output size.
    fn try_create_surface(&mut self, qh: &QueueHandle<Self>) {
        if self.surface.is_some() {
            return;
        }
        let (compositor, layer_shell) =
            match (self.compositor.as_ref(), self.layer_shell.as_ref()) {
                (Some(c), Some(l)) => (c, l),
                _ => return,
            };
        if !self.output_ready {
            return;
        }

        let surface = compositor.create_surface(qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            "scrollshot-selector".to_string(),
            qh,
            (),
        );

        layer_surface.set_size(0, 0); // let compositor decide (fullscreen)
        layer_surface.set_anchor(zwlr_layer_surface_v1::Anchor::all());
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(
            zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive,
        );
        surface.commit();

        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);
    }

    /// Allocate the mmap'd SHM buffer at canvas size.
    fn init_overlay(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let shm = match self.shm.as_ref() {
            Some(s) => s,
            None => return Ok(()),
        };
        let w = self.canvas_w;
        let h = self.canvas_h;
        if w == 0 || h == 0 {
            return Ok(());
        }

        // S3: cast before multiplying to avoid u32 overflow on very large screens.
        let buf_size = (w as usize) * (h as usize) * 4;
        let file = tempfile::tempfile()
            .context("failed to create anonymous file for overlay buffer")?;
        file.set_len(buf_size as u64)
            .context("failed to set overlay buffer size")?;

        // mmap the file so redraw() can write directly to memory
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                buf_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                std::os::unix::io::AsRawFd::as_raw_fd(&file),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(anyhow!("mmap failed for overlay buffer (errno {})", unsafe { *libc::__errno_location() }));
        }

        self.overlay_mmap = ptr as *mut u8;
        self.overlay_mmap_size = buf_size;

        // Pre-fill entire background with semi-transparent black once.
        // ARGB8888 on little-endian: 0xAARRGGBB stored as [B,G,R,A].
        // 0x88000000 → [0x00, 0x00, 0x00, 0x88] = transparent black at 53% opacity.
        let pixels_u32 = unsafe {
            std::slice::from_raw_parts_mut(ptr as *mut u32, buf_size / 4)
        };
        pixels_u32.fill(0x88000000u32);

        let pool = shm.create_pool(file.as_fd(), buf_size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );
        pool.destroy();

        self.overlay_file = Some(file);
        self.overlay_buffer = Some(buffer);
        Ok(())
    }

    /// Rate-limited redraw: if a frame is already in-flight, queue the selection
    /// and we'll pick it up in the frame callback.  Otherwise draw immediately.
    fn schedule_redraw(&mut self, qh: &QueueHandle<Self>, selection: Option<(i32, i32, i32, i32)>) {
        if self.frame_pending {
            // Overwrite with latest selection so we always show the most current state.
            self.pending_sel = Some(selection);
        } else {
            self.do_frame(qh, selection);
        }
    }

    /// Write pixels and commit one surface frame, then arm the next frame callback.
    fn do_frame(&mut self, qh: &QueueHandle<Self>, selection: Option<(i32, i32, i32, i32)>) {
        let w = self.canvas_w;
        let h = self.canvas_h;
        if w == 0 || h == 0 || self.overlay_mmap.is_null() {
            return;
        }
        let (surface, buffer) = match (self.surface.as_ref(), self.overlay_buffer.as_ref()) {
            (Some(s), Some(b)) => (s, b),
            _ => return,
        };

        let pixels =
            unsafe { std::slice::from_raw_parts_mut(self.overlay_mmap, self.overlay_mmap_size) };

        // Normalise current selection to (x1,y1,x2,y2) with x1≤x2, y1≤y2
        let cur_sel = selection.map(|(ax, ay, bx, by)| {
            let border = 3u32;
            let x1 = (ax.min(bx).max(0) as u32).saturating_sub(border);
            let y1 = (ay.min(by).max(0) as u32).saturating_sub(border);
            let x2 = (ax.max(bx) as u32 + border).min(w - 1);
            let y2 = (ay.max(by) as u32 + border).min(h - 1);
            (x1, y1, x2, y2)
        });

        // Restore background (dim) over the previous selection bbox and cursor bbox.
        if let Some((px1, py1, px2, py2)) = self.prev_sel {
            fill_rect(pixels, w, px1, py1, px2, py2, 0x88000000u32);
        }
        if let Some((px1, py1, px2, py2)) = self.prev_cursor_bbox {
            fill_rect(pixels, w, px1, py1, px2, py2, 0x88000000u32);
        }

        // Draw the new selection.
        if let Some((ax, ay, bx, by)) = selection {
            draw_selection(pixels, w, h, ax, ay, bx, by);
        }

        // Draw the crosshair cursor on top.
        let cur_cursor_bbox = self.cursor_pos.map(|(cx, cy)| {
            draw_crosshair(pixels, w, h, cx, cy);
            crosshair_bbox(cx, cy, w, h)
        });

        // Compute damage = union of all changed bounding boxes.
        let rects: Vec<(u32, u32, u32, u32)> = [
            self.prev_sel,
            cur_sel,
            self.prev_cursor_bbox,
            cur_cursor_bbox,
        ]
        .into_iter()
        .flatten()
        .collect();

        let (dx, dy, dw, dh) = if rects.is_empty() {
            (0, 0, w as i32, h as i32)
        } else {
            let ux1 = rects.iter().map(|r| r.0).min().unwrap() as i32;
            let uy1 = rects.iter().map(|r| r.1).min().unwrap() as i32;
            let ux2 = rects.iter().map(|r| r.2).max().unwrap() as i32;
            let uy2 = rects.iter().map(|r| r.3).max().unwrap() as i32;
            (ux1, uy1, ux2 - ux1 + 1, uy2 - uy1 + 1)
        };

        self.prev_sel = cur_sel;
        self.prev_cursor_bbox = cur_cursor_bbox;

        // Request a frame callback so we know when the compositor has consumed this
        // commit and we can safely send the next one (vsync rate-limiting).
        self.frame_callback = Some(surface.frame(qh, ()));
        self.frame_pending = true;

        surface.attach(Some(buffer), 0, 0);
        surface.damage(dx, dy, dw, dh);
        surface.commit();
    }

    fn current_selection(&self) -> Option<(i32, i32, i32, i32)> {
        Some((
            self.drag_start?.0 as i32,
            self.drag_start?.1 as i32,
            self.drag_end?.0 as i32,
            self.drag_end?.1 as i32,
        ))
    }

    fn result_rect(&self) -> Option<Rect> {
        let (x1, y1, x2, y2) = self.current_selection()?;
        let (x1, x2) = (x1.min(x2), x1.max(x2));
        let (y1, y2) = (y1.min(y2), y1.max(y2));
        if x2 - x1 < 2 || y2 - y1 < 2 {
            return None;
        }
        Some(Rect { x: x1, y: y1, width: (x2 - x1) as u32, height: (y2 - y1) as u32 })
    }
}

// ── Drawing helpers ───────────────────────────────────────────────────────────

/// Fill a rectangle with a solid u32 colour (ARGB8888 little-endian).
fn fill_rect(pixels: &mut [u8], w: u32, x1: u32, y1: u32, x2: u32, y2: u32, colour: u32) {
    let row_bytes = (w * 4) as usize;
    let cb = colour.to_ne_bytes();
    for y in y1..=y2 {
        let row_start = y as usize * row_bytes;
        let start = row_start + x1 as usize * 4;
        let end = row_start + (x2 + 1) as usize * 4;
        if end > pixels.len() { break; }
        // Fill row span using u32 writes via chunks_exact_mut
        let span = &mut pixels[start..end];
        for chunk in span.chunks_exact_mut(4) {
            chunk.copy_from_slice(&cb);
        }
    }
}

/// Draw the selection rectangle: transparent interior + 2-pixel white border.
fn draw_selection(pixels: &mut [u8], w: u32, h: u32, ax: i32, ay: i32, bx: i32, by: i32) {
    let x1 = ax.min(bx).max(0) as u32;
    let y1 = ay.min(by).max(0) as u32;
    let x2 = (ax.max(bx) as u32).min(w.saturating_sub(1));
    let y2 = (ay.max(by) as u32).min(h.saturating_sub(1));

    // Transparent cutout
    fill_rect(pixels, w, x1, y1, x2, y2, 0x00000000u32);

    // White border (2 px outside the cutout)
    let border = 2u32;
    let bx1 = x1.saturating_sub(border);
    let by1 = y1.saturating_sub(border);
    let bx2 = (x2 + border).min(w - 1);
    let by2 = (y2 + border).min(h - 1);
    let white = 0xFFFFFFFFu32;

    // Top and bottom edges
    fill_rect(pixels, w, bx1, by1, bx2, by1 + border - 1, white);
    fill_rect(pixels, w, bx1, by2 - border + 1, bx2, by2, white);
    // Left and right edges (between top and bottom)
    fill_rect(pixels, w, bx1, by1 + border, bx1 + border - 1, by2 - border, white);
    fill_rect(pixels, w, bx2 - border + 1, by1 + border, bx2, by2 - border, white);
}

/// Draw a crosshair (+) cursor at (cx, cy) in white.
fn draw_crosshair(pixels: &mut [u8], w: u32, h: u32, cx: u32, cy: u32) {
    let arm: u32 = 14; // half-length of each arm in pixels
    let gap: u32 = 3;  // gap around the centre point
    let thick: u32 = 1; // half-thickness (total line width = 2*thick+1 = 3px)
    let colour = 0xFFFFFFFFu32; // opaque white

    // Horizontal arms (left and right of centre gap)
    let y0 = cy.saturating_sub(thick);
    let y1 = (cy + thick).min(h.saturating_sub(1));
    // left arm
    let lx0 = cx.saturating_sub(arm);
    let lx1 = cx.saturating_sub(gap + 1);
    if lx1 >= lx0 { fill_rect(pixels, w, lx0, y0, lx1, y1, colour); }
    // right arm
    let rx0 = (cx + gap + 1).min(w.saturating_sub(1));
    let rx1 = (cx + arm).min(w.saturating_sub(1));
    if rx1 >= rx0 { fill_rect(pixels, w, rx0, y0, rx1, y1, colour); }

    // Vertical arms (above and below centre gap)
    let x0 = cx.saturating_sub(thick);
    let x1 = (cx + thick).min(w.saturating_sub(1));
    // top arm
    let ty0 = cy.saturating_sub(arm);
    let ty1 = cy.saturating_sub(gap + 1);
    if ty1 >= ty0 { fill_rect(pixels, w, x0, ty0, x1, ty1, colour); }
    // bottom arm
    let by0 = (cy + gap + 1).min(h.saturating_sub(1));
    let by1 = (cy + arm).min(h.saturating_sub(1));
    if by1 >= by0 { fill_rect(pixels, w, x0, by0, x1, by1, colour); }
}

/// Bounding box of the crosshair for incremental damage tracking.
fn crosshair_bbox(cx: u32, cy: u32, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let arm: u32 = 14;
    let x1 = cx.saturating_sub(arm);
    let y1 = cy.saturating_sub(arm);
    let x2 = (cx + arm).min(w.saturating_sub(1));
    let y2 = (cy + arm).min(h.saturating_sub(1));
    (x1, y1, x2, y2)
}

// ── Dispatch impls ────────────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for SelectorState {
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
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                    state.try_create_surface(qh);
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "wl_seat" => {
                    let seat: wl_seat::WlSeat = registry.bind(name, version.min(7), qh, ());
                    state.seat = Some(seat);
                }
                "wl_output" => {
                    let _: wl_output::WlOutput = registry.bind(name, version.min(3), qh, ());
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                    state.try_create_surface(qh);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for SelectorState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Mode { width, height, .. } = event {
            if width > 0 && height > 0 {
                state.screen_w = width as u32;
                state.screen_h = height as u32;
                state.output_ready = true;
                state.try_create_surface(qh);
            }
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for SelectorState {
    fn event(
        state: &mut Self,
        layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, width, height } = event {
            layer_surface.ack_configure(serial);
            // Compositor tells us the actual surface size
            state.canvas_w = if width > 0 { width } else { state.screen_w };
            state.canvas_h = if height > 0 { height } else { state.screen_h };
            // Allocate buffer at the correct size and draw the initial overlay.
            if let Err(e) = state.init_overlay(qh) {
                state.init_error = Some(format!("{e:#}"));
                state.done = true;
                return;
            }
            state.schedule_redraw(qh, None);
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for SelectorState {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for SelectorState {
    fn event(
        state: &mut Self,
        pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                // Hide the compositor's cursor — we draw our own crosshair.
                pointer.set_cursor(serial, None, 0, 0);
                // Initialise cursor position on enter so the crosshair shows
                // immediately even before the first Motion event.
                state.cursor_pos = Some((surface_x as u32, surface_y as u32));
                if state.canvas_w > 0 {
                    let sel = state.current_selection();
                    state.schedule_redraw(qh, sel);
                }
            }
            wl_pointer::Event::Button { button, state: WEnum::Value(btn_state), .. } => {
                if button == BTN_RIGHT {
                    state.cancelled = true;
                    state.done = true;
                    return;
                }
                if button == BTN_LEFT {
                    match btn_state {
                        wl_pointer::ButtonState::Pressed => {
                            state.pressing = true;
                            if let Some(cur) = state.drag_end {
                                state.drag_start = Some(cur);
                            }
                        }
                        wl_pointer::ButtonState::Released => {
                            state.pressing = false;
                            state.done = true;
                        }
                        _ => {}
                    }
                }
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                state.cursor_pos = Some((surface_x as u32, surface_y as u32));
                state.drag_end = Some((surface_x, surface_y));
                if !state.pressing {
                    // Keep start in sync so first press picks up current position.
                    state.drag_start = Some((surface_x, surface_y));
                }
                if state.canvas_w > 0 {
                    let sel = state.current_selection();
                    state.schedule_redraw(qh, sel);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for SelectorState {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key: KEY_ESC,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            state.cancelled = true;
            state.done = true;
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for SelectorState {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            // The previous frame has been consumed by the compositor.
            state.frame_pending = false;
            state.frame_callback = None;
            // If a redraw was queued while we were waiting, dispatch it now.
            if let Some(sel) = state.pending_sel.take() {
                state.do_frame(qh, sel);
            }
        }
    }
}

impl Drop for SelectorState {
    fn drop(&mut self) {
        if !self.overlay_mmap.is_null() {
            unsafe { libc::munmap(self.overlay_mmap as *mut libc::c_void, self.overlay_mmap_size) };
        }
    }
}

delegate_noop!(SelectorState: ignore wl_compositor::WlCompositor);
delegate_noop!(SelectorState: ignore wl_surface::WlSurface);
delegate_noop!(SelectorState: ignore wl_shm::WlShm);
delegate_noop!(SelectorState: ignore wl_shm_pool::WlShmPool);
delegate_noop!(SelectorState: ignore wl_buffer::WlBuffer);
delegate_noop!(SelectorState: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);

// ── Public entry point ────────────────────────────────────────────────────────

pub fn select_region() -> Result<Option<Rect>> {
    let conn = Connection::connect_to_env()
        .map_err(|e| anyhow!("Cannot connect to Wayland display: {e}"))?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();

    conn.display().get_registry(&qh, ());
    let mut state = SelectorState::new();

    // First roundtrip: get global list
    queue.roundtrip(&mut state)?;

    if state.layer_shell.is_none() {
        return Err(anyhow!(
            "zwlr_layer_shell_v1 not available — is your compositor wlroots-based?"
        ));
    }

    // Second roundtrip: get output geometry + initial events
    queue.roundtrip(&mut state)?;

    // Run event loop until user selects or cancels
    while !state.done {
        queue.blocking_dispatch(&mut state)?;
    }

    if let Some(msg) = &state.init_error {
        return Err(anyhow!("{msg}"));
    }
    if state.cancelled {
        return Ok(None);
    }
    Ok(state.result_rect())
}
