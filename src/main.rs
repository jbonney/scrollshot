mod selector;
mod screencopy;
mod stitch;

use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

fn main() -> Result<()> {
    let output_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            PathBuf::from(format!("scrollshot_{}.png", ts))
        });

    eprintln!("Click and drag to select a region (right-click or ESC to cancel)...");
    let region = match selector::select_region()? {
        Some(r) if r.width > 0 && r.height > 0 => r,
        _ => {
            eprintln!("No region selected.");
            return Ok(());
        }
    };

    eprintln!(
        "Selected {}x{} at ({}, {}). Capturing...",
        region.width, region.height, region.x, region.y
    );

    // Small delay so the overlay is fully gone before we start scrolling
    std::thread::sleep(std::time::Duration::from_millis(150));

    let (frames, diffs) = screencopy::capture_scrolling(region)?;
    eprintln!("Captured {} frames", frames.len());

    if frames.is_empty() {
        eprintln!("No frames captured.");
        return Ok(());
    }

    eprintln!("Stitching...");
    let result = stitch::stitch_frames(frames, diffs)?;
    result.save(&output_path)?;
    eprintln!("Saved to: {}", output_path.display());

    Ok(())
}
