//! Stitch a sequence of overlapping frames into one tall image.
//!
//! Algorithm:
//!   1. **Scroll detection** via row-by-row voting: for every row in `prev`,
//!      find the best-matching row in `next` (full-width SAD, sampled every
//!      4th pixel).  Each match implies a scroll value; the most-voted scroll
//!      wins.  With ~600+ content rows voting, ambiguous blank/separator rows
//!      are overwhelmed by unique content rows (player names, text, etc.).
//!
//!   2. **Seam finding**: within the overlap region, find the row where both
//!      frames are most pixel-similar (full-width SAD) and cut there, hiding
//!      any rendering differences at the stitch boundary.

use anyhow::{anyhow, Result};
use image::RgbaImage;
use std::collections::HashMap;

/// Minimum number of row votes required to trust a scroll offset detection.
const MIN_SCROLL_VOTES: usize = 5;

// ── Row comparison helpers ───────────────────────────────────────────────────

/// Full-width SAD between one row of `a` and one row of `b`.
fn row_sad_full(a: &RgbaImage, ay: u32, b: &RgbaImage, by: u32) -> u64 {
    let w = a.width().min(b.width());
    let mut sad = 0u64;
    for x in 0..w {
        let pa = a.get_pixel(x, ay);
        let pb = b.get_pixel(x, by);
        sad += (pa[0] as i64 - pb[0] as i64).unsigned_abs();
        sad += (pa[1] as i64 - pb[1] as i64).unsigned_abs();
        sad += (pa[2] as i64 - pb[2] as i64).unsigned_abs();
    }
    sad
}

/// SAD between one row of `a` and one row of `b`, sampling every 4th pixel.
fn row_sad_fast(a: &RgbaImage, ay: u32, b: &RgbaImage, by: u32) -> u64 {
    let w = a.width().min(b.width());
    let mut sad = 0u64;
    let mut x = 0u32;
    while x < w {
        let pa = a.get_pixel(x, ay);
        let pb = b.get_pixel(x, by);
        sad += (pa[0] as i64 - pb[0] as i64).unsigned_abs();
        sad += (pa[1] as i64 - pb[1] as i64).unsigned_abs();
        sad += (pa[2] as i64 - pb[2] as i64).unsigned_abs();
        x += 4;
    }
    sad
}

// ── Scroll offset detection ────────────────────────────────────────────────

/// Find scroll offset by scanning ALL rows of prev against ALL rows of next,
/// then voting.  Content rows (with unique text/graphics) vastly outnumber
/// ambiguous blank/separator rows, so the correct offset always wins.
fn find_scroll_offset(prev: &RgbaImage, next: &RgbaImage) -> Option<u32> {
    let h = prev.height().min(next.height()) as usize;
    let w = prev.width().min(next.width()) as usize;
    if h < 32 || w < 8 {
        return None;
    }

    let sampled_pixels = (w / 4) * 3;
    let threshold = sampled_pixels as u64; // avg SAD < 1.0 per sampled channel

    let mut votes: HashMap<usize, usize> = HashMap::new();

    for py in 0..h {
        let mut best_ny = 0usize;
        let mut best_sad = u64::MAX;

        for ny in 0..h {
            let sad = row_sad_fast(prev, py as u32, next, ny as u32);
            if sad < best_sad {
                best_sad = sad;
                best_ny = ny;
            }
        }

        if best_sad < threshold && py > best_ny {
            let s = py - best_ny;
            *votes.entry(s).or_insert(0) += 1;
        }
    }

    let mut sorted: Vec<(usize, usize)> = votes.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    // Log top candidates
    for &(s, count) in sorted.iter().take(3) {
        eprintln!("    scroll={} votes={}", s, count);
    }

    match sorted.first() {
        Some(&(s, count)) if count >= MIN_SCROLL_VOTES => {
            eprintln!("  stitch: scroll={} ({} votes)", s, count);
            Some(s as u32)
        }
        _ => {
            eprintln!("  stitch: no reliable scroll found");
            None
        }
    }
}

// ── Seam finding ─────────────────────────────────────────────────────────────

/// Find the best seam row within the overlap region where both frames are most
/// pixel-similar.  Returns the cut row in next-frame coordinates.
fn find_seam(prev: &RgbaImage, next: &RgbaImage, scroll: u32) -> u32 {
    let h = prev.height().min(next.height()) as usize;
    let s = scroll as usize;
    let overlap = h.saturating_sub(s);
    if overlap < 4 {
        return 0;
    }

    // Search the middle 80% of the overlap (avoid edges with capture artifacts).
    let margin = overlap / 10;
    let search_start = margin.max(1);
    let search_end = (overlap - margin).min(overlap);

    let mut best_row = search_start;
    let mut best_sad = u64::MAX;

    for r in search_start..search_end {
        let sad = row_sad_full(prev, (s + r) as u32, next, r as u32);
        if sad < best_sad {
            best_sad = sad;
            best_row = r;
        }
    }

    eprintln!("  stitch: seam at row {} (sad={})", best_row, best_sad);
    best_row as u32
}

// ── Public entry point ─────────────────────────────────────────────────────

pub fn stitch_frames(frames: Vec<RgbaImage>) -> Result<RgbaImage> {
    if frames.is_empty() {
        return Err(anyhow!("no frames to stitch"));
    }
    if frames.len() == 1 {
        return Ok(frames.into_iter().next().unwrap());
    }

    let frame_w = frames[0].width();
    let frame_h = frames[0].height() as usize;

    // Debug: save each raw frame for inspection (opt-in via SCROLLSHOT_DEBUG=1).
    if std::env::var_os("SCROLLSHOT_DEBUG").is_some() {
        for (i, frame) in frames.iter().enumerate() {
            let path = format!("frame_{}.png", i + 1);
            if let Err(e) = frame.save(&path) {
                eprintln!("  stitch: could not save {}: {}", path, e);
            }
        }
    }

    // Step 1: find scroll offsets and seam rows for each consecutive pair.
    let mut scrolls: Vec<usize> = Vec::new();
    let mut seams: Vec<usize> = Vec::new();

    for i in 0..frames.len() - 1 {
        eprintln!("  stitch: --- frame {} -> {} ---", i + 1, i + 2);

        let scroll = find_scroll_offset(&frames[i], &frames[i + 1])
            .unwrap_or_else(|| {
                // Fallback: use last good scroll, or frame_h / 4
                let fb = scrolls.last().copied().unwrap_or(frame_h / 4);
                eprintln!("  stitch: fallback scroll={}", fb);
                fb as u32
            }) as usize;

        let seam = find_seam(&frames[i], &frames[i + 1], scroll as u32) as usize;

        scrolls.push(scroll);
        seams.push(seam);
    }

    eprintln!("  stitch: scrolls={:?}", scrolls);
    eprintln!("  stitch: seams={:?}", seams);

    // Step 2: build slices using seam-based cuts.
    // For each pair (frame i, frame i+1) with scroll s and seam r:
    //   - frame i provides rows up to (s + r) in its own coordinates
    //   - frame i+1 starts at row r in its own coordinates

    struct Slice {
        frame: usize,
        start: usize,
        end: usize,
    }

    let first_end = scrolls[0] + seams[0];
    let mut slices: Vec<Slice> = vec![Slice {
        frame: 0,
        start: 0,
        end: first_end.min(frame_h),
    }];

    for i in 0..scrolls.len() {
        let is_last = i == scrolls.len() - 1;
        let start = seams[i];
        let end = if is_last {
            frame_h
        } else {
            (scrolls[i + 1] + seams[i + 1]).min(frame_h)
        };
        slices.push(Slice {
            frame: i + 1,
            start,
            end,
        });
    }

    for sl in &slices {
        eprintln!(
            "  stitch: frame {} rows {}..{} ({} rows)",
            sl.frame + 1,
            sl.start,
            sl.end,
            sl.end - sl.start
        );
    }

    // Step 3: assemble output image.
    let total_h: usize = slices.iter().map(|sl| sl.end - sl.start).sum();
    let mut out = RgbaImage::new(frame_w, total_h as u32);

    let mut y_out = 0u32;
    for sl in &slices {
        for r in sl.start..sl.end {
            if r >= frames[sl.frame].height() as usize || y_out >= total_h as u32 {
                break;
            }
            for x in 0..frame_w.min(frames[sl.frame].width()) {
                out.put_pixel(x, y_out, *frames[sl.frame].get_pixel(x, r as u32));
            }
            y_out += 1;
        }
    }

    eprintln!("  stitch: output {}x{}", frame_w, total_h);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// Build an image where every row has a unique colour derived from
    /// `(row + offset) % 256`.  This guarantees unambiguous row matching,
    /// which makes scroll detection deterministic in tests.
    fn striped(width: u32, height: u32, offset: u32) -> RgbaImage {
        let mut img = RgbaImage::new(width, height);
        for y in 0..height {
            let v = ((y + offset) % 256) as u8;
            for x in 0..width {
                img.put_pixel(x, y, Rgba([v, 255 - v, v / 2, 255]));
            }
        }
        img
    }

    // ── row_sad helpers ──────────────────────────────────────────────────────

    #[test]
    fn row_sad_identical_rows_is_zero() {
        let img = striped(64, 32, 0);
        assert_eq!(row_sad_full(&img, 0, &img, 0), 0);
        assert_eq!(row_sad_fast(&img, 5, &img, 5), 0);
    }

    #[test]
    fn row_sad_different_rows_is_nonzero() {
        let img = striped(64, 32, 0);
        assert!(row_sad_full(&img, 0, &img, 1) > 0);
        assert!(row_sad_fast(&img, 0, &img, 1) > 0);
    }

    #[test]
    fn row_sad_fast_le_full_for_same_rows() {
        // fast samples every 4th pixel so it can only be <= full
        let a = striped(64, 32, 0);
        let b = striped(64, 32, 7);
        assert!(row_sad_fast(&a, 10, &b, 10) <= row_sad_full(&a, 10, &b, 10));
    }

    // ── find_scroll_offset ───────────────────────────────────────────────────

    #[test]
    fn scroll_offset_detects_exact_shift() {
        let scroll = 30u32;
        // next is prev shifted up by `scroll` rows
        let prev = striped(64, 100, 0);
        let next = striped(64, 100, scroll);
        assert_eq!(find_scroll_offset(&prev, &next), Some(scroll));
    }

    #[test]
    fn scroll_offset_rejects_image_too_narrow() {
        let narrow = striped(4, 100, 0); // width < 8
        let normal = striped(64, 100, 0);
        assert_eq!(find_scroll_offset(&narrow, &normal), None);
    }

    #[test]
    fn scroll_offset_rejects_image_too_short() {
        let short = striped(64, 10, 0); // height < 32
        assert_eq!(find_scroll_offset(&short, &short), None);
    }

    #[test]
    fn scroll_offset_returns_none_for_identical_frames() {
        // Identical frames → every row matches its counterpart at offset 0,
        // but the condition `py > best_ny` is never true → no votes → None.
        let img = striped(64, 100, 0);
        assert_eq!(find_scroll_offset(&img, &img), None);
    }

    // ── find_seam ────────────────────────────────────────────────────────────

    #[test]
    fn seam_lies_within_middle_80_percent_of_overlap() {
        let scroll = 30u32;
        let height = 100u32;
        let prev = striped(64, height, 0);
        let next = striped(64, height, scroll);

        let seam = find_seam(&prev, &next, scroll);
        let overlap = height - scroll; // 70
        let margin = overlap / 10;     // 7
        assert!(seam >= margin, "seam {seam} below margin {margin}");
        assert!(seam < overlap - margin, "seam {seam} at or above {}", overlap - margin);
    }

    #[test]
    fn seam_returns_zero_when_overlap_too_small() {
        let img = striped(64, 100, 0);
        // scroll = 98 → overlap = 2 < 4
        assert_eq!(find_seam(&img, &img, 98), 0);
    }

    // ── stitch_frames ────────────────────────────────────────────────────────

    #[test]
    fn stitch_single_frame_passthrough() {
        let img = striped(64, 100, 0);
        let result = stitch_frames(vec![img.clone()]).unwrap();
        assert_eq!(result.dimensions(), img.dimensions());
        assert_eq!(result.get_pixel(32, 50), img.get_pixel(32, 50));
    }

    #[test]
    fn stitch_empty_frames_is_error() {
        assert!(stitch_frames(vec![]).is_err());
    }

    #[test]
    fn stitch_two_frames_height_equals_height_plus_scroll() {
        let scroll = 30u32;
        let height = 100u32;
        let width = 64u32;
        let prev = striped(width, height, 0);
        let next = striped(width, height, scroll);

        let result = stitch_frames(vec![prev, next]).unwrap();

        // Total rows = first_slice (scroll + seam) + second_slice (height - seam)
        //            = scroll + height, regardless of seam position.
        assert_eq!(result.width(), width);
        assert_eq!(result.height(), height + scroll);
    }

    #[test]
    fn stitch_two_frames_pixel_continuity() {
        // The top of the output should come from frame 0 (offset 0) and the
        // bottom should come from frame 1 (offset = scroll), so the colour at
        // any row should equal its y-coordinate modulo 256.
        let scroll = 30u32;
        let height = 100u32;
        let prev = striped(64, height, 0);
        let next = striped(64, height, scroll);

        let result = stitch_frames(vec![prev, next]).unwrap();

        for y in 0..result.height() {
            let expected_v = (y % 256) as u8;
            let pixel = result.get_pixel(0, y);
            assert_eq!(
                pixel[0], expected_v,
                "wrong colour at row {y}: got {} expected {expected_v}",
                pixel[0]
            );
        }
    }
}
