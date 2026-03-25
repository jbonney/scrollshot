mod selector;
mod screencopy;
mod stitch;

use anyhow::{anyhow, Result};
use clap::Parser;
use image::open;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Wayland scrolling screenshot tool for wlroots-based compositors.
#[derive(Parser)]
#[command(version, after_help = "\
Examples:
  scrollshot                          Capture and save to scrollshot_{timestamp}.png
  scrollshot output.png               Capture and save to output.png
  scrollshot -o output.png            Same, with explicit flag
  scrollshot -i ./frames/             Stitch pre-captured frames from a directory
  scrollshot -i ./frames/ -o out.png  Stitch frames with explicit output
  scrollshot -i ./frames/ --cleanup   Stitch frames and delete the directory afterwards
  scrollshot --capture-only ./frames/ Capture frames only, skip stitching
  scrollshot --scroll-delay 300       Wait 300ms between scrolls (default 200)
  scrollshot --scroll-ticks 3         3 scroll ticks per step (default 2)
  scrollshot --debug                  Save raw frames as frame_N.png for inspection")]
struct Cli {
    /// Stitch frames from a directory instead of capturing
    #[arg(short, long, value_name = "DIR")]
    input: Option<PathBuf>,

    /// Output file path
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Capture frames and save to DIR without stitching
    #[arg(long, value_name = "DIR")]
    capture_only: Option<PathBuf>,

    /// Milliseconds to wait after each scroll for the page to re-render
    #[arg(long, default_value_t = 200)]
    scroll_delay: u64,

    /// Discrete scroll wheel ticks per step
    #[arg(long, default_value_t = 2)]
    scroll_ticks: i32,

    /// Delete the input frames directory after stitching (only with -i)
    #[arg(long)]
    cleanup: bool,

    /// Save raw capture frames as frame_N.png for debugging
    #[arg(long)]
    debug: bool,

    /// Output file path (positional, overridden by --output)
    #[arg(value_name = "OUTPUT")]
    positional_output: Option<PathBuf>,
}

impl Cli {
    fn output_path(&self) -> PathBuf {
        self.output
            .clone()
            .or_else(|| self.positional_output.clone())
            .unwrap_or_else(|| {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                PathBuf::from(format!("scrollshot_{}.png", ts))
            })
    }
}

fn load_frames(dir: &PathBuf) -> Result<Vec<image::RgbaImage>> {
    let mut frames = Vec::new();
    for i in 1.. {
        let path = dir.join(format!("frame_{}.png", i));
        match open(&path) {
            Ok(img) => {
                eprintln!("Loading {}", path.display());
                frames.push(img.into_rgba8());
            }
            Err(_) => break,
        }
    }
    if frames.is_empty() {
        return Err(anyhow!("no frame_N.png files found in {}", dir.display()));
    }
    Ok(frames)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let output_path = cli.output_path();

    let frames = if let Some(ref input_dir) = cli.input {
        load_frames(input_dir)?
    } else {
        eprintln!("Click and drag to select a region (right-click or ESC to cancel)...");
        let (region, output_global) = match selector::select_region()? {
            Some((r, o)) if r.width > 0 && r.height > 0 => (r, o),
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

        let captured = screencopy::capture_scrolling(
            region,
            output_global,
            cli.scroll_delay,
            cli.scroll_ticks,
        )?;
        eprintln!("Captured {} frames", captured.len());

        if captured.is_empty() {
            eprintln!("No frames captured.");
            return Ok(());
        }
        captured
    };

    if let Some(ref frames_dir) = cli.capture_only {
        std::fs::create_dir_all(frames_dir)?;
        for (i, frame) in frames.iter().enumerate() {
            let path = frames_dir.join(format!("frame_{}.png", i + 1));
            frame.save(&path)?;
            eprintln!("Saved {}", path.display());
        }
        eprintln!("Capture complete ({} frames). Stitch with: scrollshot -i {}", frames.len(), frames_dir.display());
        return Ok(());
    }

    if cli.debug || std::env::var_os("SCROLLSHOT_DEBUG").is_some() {
        for (i, frame) in frames.iter().enumerate() {
            let path = format!("frame_{}.png", i + 1);
            if let Err(e) = frame.save(&path) {
                eprintln!("  could not save {}: {}", path, e);
            }
        }
    }

    eprintln!("Stitching...");
    let result = stitch::stitch_frames(frames)?;
    result.save(&output_path)?;
    eprintln!("Saved to: {}", output_path.display());

    if cli.cleanup {
        if let Some(ref input_dir) = cli.input {
            std::fs::remove_dir_all(input_dir)?;
        } else {
            eprintln!("Warning: --cleanup has no effect without -i");
        }
    }

    Ok(())
}
