# scrollshot

A scrolling screenshot tool for Wayland (wlroots-based compositors). Select a region, and it automatically scrolls and captures the full page content into a single tall PNG.

## Requirements

- A wlroots-based Wayland compositor (Sway, Hyprland, etc.)
- The following protocols must be supported:
  - `zwlr_layer_shell_v1`
  - `zwlr_screencopy_manager_v1`
  - `zwlr_virtual_pointer_manager_v1`

Will **not** work on GNOME or KDE compositors.

## Build

```bash
cargo build --release
```

## Usage

```bash
./target/release/scrollshot
```

1. A fullscreen overlay appears — click and drag to select the region to capture.
2. The tool auto-scrolls the content and captures frames.
3. Frames are stitched into a single image and saved as `test.png`.

Right-click or press ESC to cancel during selection.

## How it works

1. **Region selection** — A transparent overlay (layer-shell) lets you draw a rectangle over the area to capture.
2. **Capture loop** — Scrolls down using virtual pointer events (2 scroll ticks per step), waits 200ms for rendering, then captures via screencopy. Stops when content stops changing.
3. **Stitching** — Detects scroll offsets between frames using row-by-row voting (every row in the previous frame is matched against the next frame; the most-voted offset wins). Cuts are placed at the row where both frames are most pixel-similar, hiding seam artifacts.

## Acknowledgements

The stitching algorithm was developed after studying approaches from [ShareX](https://github.com/ShareX/ShareX), [wayscrollshot](https://github.com/jswysnemc/wayscrollshot), and [long-shot-rs](https://github.com/jswysnemc/long-shot-rs).
