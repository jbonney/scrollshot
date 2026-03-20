# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
cargo build --release          # main binary
cargo build --release --bin test_stitch  # standalone stitch tester

./target/release/scrollshot                      # capture, stitch, save to scrollshot_{ts}.png
./target/release/scrollshot -o out.png           # capture with explicit output path
./target/release/scrollshot -i ./frames/         # stitch pre-captured frames from directory
./target/release/scrollshot --debug              # save raw frames as frame_N.png
./target/release/scrollshot --scroll-delay 300   # slower settle (default 200ms)
./target/release/scrollshot --scroll-ticks 3     # more scroll per step (default 2)
./target/release/test_stitch   # standalone stitch tester (loads frame_N.png from cwd)
```

No tests or linter configured.

## Architecture

Wayland scrolling screenshot tool for wlroots-based compositors. Three-stage pipeline:

1. **selector.rs** — Layer-shell overlay for click-drag region selection. Uses mmap'd SHM buffer with incremental damage tracking for efficient rendering. Returns a `Rect`.

2. **screencopy.rs** — Capture loop. Scrolls via `zwlr_virtual_pointer` (configurable ticks/delay via CLI), captures frames via `zwlr_screencopy_manager`. Stops after 2 consecutive unchanged frames. Returns `Vec<RgbaImage>`.

3. **stitch.rs** — Merges overlapping frames. Two-phase algorithm:
   - **Scroll detection**: For every row in prev frame, find best-matching row in next (sampled SAD), vote on implied offset. Content rows overwhelm ambiguous blank rows.
   - **Seam finding**: Within overlap, cut at the row where both frames are most pixel-similar (full-width SAD, middle 80% of overlap).
   - Slices are assembled: frame 1 up to `scroll+seam`, each subsequent frame from its seam row to the next frame's `scroll+seam`, last frame to bottom.

**bin/test_stitch.rs** — Standalone offline stitcher for algorithm development. Loads `frame_N.png` files, applies same algorithm, saves `stitched.png`.

## Wayland Protocols

Requires wlroots extensions: `zwlr_layer_shell_v1`, `zwlr_screencopy_manager_v1`, `zwlr_virtual_pointer_manager_v1`. Will not work on GNOME/KDE without these protocols.

## Stitching Algorithm References

The row-voting approach was developed after evaluating algorithms from:
- **ShareX** (`ScrollingCaptureManager.cs`) — binary row matching with consecutive-match counting
- **wayscrollshot** (`src/stitch.rs`) — column sampling, NCC template matching, FAST corner voting with ambiguity detection
- **long-shot-rs** — OpenCV template matching on Sobel-filtered images

ShareX's exact-row matching and wayscrollshot's voting/ambiguity concepts influenced the final design. NCC and fixed-strip approaches were tried and failed on pages with repeating card layouts; the all-rows voting approach proved robust across all test cases.

## Key Design Decisions

- Row-voting scroll detection handles repetitive page layouts (similar cards, tables) where template/window matching fails — tested on pages with 3 identical game card structures.
- SHM pixel format conversion handles both XRGB8888 and XBGR8888 byte orders.

## Debug Mode

Use `--debug` (or set `SCROLLSHOT_DEBUG=1`) to save raw capture frames as `frame_N.png` in the current working directory before stitching:

```bash
scrollshot --debug
```

These files can then be re-stitched offline with `scrollshot -i .` or `test_stitch`.
