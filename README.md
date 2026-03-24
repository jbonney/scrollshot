# scrollshot

A scrolling screenshot tool for Wayland (wlroots-based compositors). Select a region, and it automatically scrolls and captures the full page content into a single tall PNG.

## Requirements

- A wlroots-based Wayland compositor (Sway, Hyprland, etc.)
- The following protocols must be supported:
  - `zwlr_layer_shell_v1`
  - `zwlr_screencopy_manager_v1`
  - `zwlr_virtual_pointer_manager_v1`

Will **not** work on GNOME or KDE compositors.

## Installation

### Arch Linux (AUR)

```bash
yay -S scrollshot-git
```

Or manually:

```bash
git clone https://github.com/jbonney/scrollshot
cd scrollshot
makepkg -si
```

### From source

```bash
cargo build --release
```

## Usage

```bash
scrollshot                          # capture and save to scrollshot_{timestamp}.png
scrollshot output.png               # capture and save to output.png
scrollshot -o output.png            # same, with explicit flag
scrollshot -i ./frames/             # stitch pre-captured frames from a directory
scrollshot -i ./frames/ -o out.png  # stitch frames with explicit output
scrollshot --scroll-delay 300       # wait 300ms between scrolls (default 200)
scrollshot --scroll-ticks 3         # 3 scroll ticks per step (default 2)
scrollshot --debug                  # save raw frames as frame_N.png for inspection
```

### Options

| Flag | Description |
|---|---|
| `-o, --output <FILE>` | Output file path (default: `scrollshot_{timestamp}.png`) |
| `-i, --input <DIR>` | Stitch `frame_N.png` files from a directory instead of capturing |
| `--scroll-delay <MS>` | Milliseconds to wait after each scroll for re-render (default: 200) |
| `--scroll-ticks <N>` | Discrete scroll wheel ticks per step (default: 2) |
| `--debug` | Save raw capture frames as `frame_N.png` before stitching |

### Workflow

1. A fullscreen overlay appears — click and drag to select the region to capture.
2. The tool auto-scrolls the content and captures frames.
3. Frames are stitched into a single image and saved.

Right-click or press ESC to cancel during selection.

Use `--debug` to save raw frames, then iterate on stitching with `scrollshot -i .` without re-capturing.

## How it works

1. **Region selection** — A transparent overlay (layer-shell) lets you draw a rectangle over the area to capture.
2. **Capture loop** — Scrolls down using virtual pointer events, waits for rendering, then captures via screencopy. Scroll speed and settle time are configurable via `--scroll-ticks` and `--scroll-delay`. Stops when content stops changing.
3. **Stitching** — Detects scroll offsets between frames using row-by-row voting (every row in the previous frame is matched against the next frame; the most-voted offset wins). Cuts are placed at the row where both frames are most pixel-similar, hiding seam artifacts.

## Limitations

The stitched result may contain artifacts depending on the page content:

- **Repeated content** — Pages with many similar elements (identical cards, table rows) can cause the algorithm to misalign, duplicating some sections.
- **Missing whitespace** — Large blank areas or empty lines may be partially collapsed, since they look identical across frames.

If you notice issues, try adjusting `--scroll-delay` or `--scroll-ticks`, and use `--debug` to inspect the raw frames and verify all content was captured.

## Built with Claude

This entire application was generated using [Claude](https://claude.ai) (Anthropic's AI assistant) via [Claude Code](https://claude.ai/claude-code).

## Acknowledgements

The stitching algorithm was developed after studying approaches from [ShareX](https://github.com/ShareX/ShareX), [wayscrollshot](https://github.com/jswysnemc/wayscrollshot), and [long-shot-rs](https://github.com/jswysnemc/long-shot-rs).
