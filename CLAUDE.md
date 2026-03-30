# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
cargo build --release          # main binary

./target/release/scrollshot                           # capture, stitch, save to scrollshot_{ts}.png
./target/release/scrollshot -o out.png                # capture with explicit output path
./target/release/scrollshot -i ./frames/              # stitch pre-captured frames from directory
./target/release/scrollshot -i ./frames/ --cleanup    # stitch and delete frames directory afterwards
./target/release/scrollshot --capture-only ./frames/  # capture frames only, skip stitching
./target/release/scrollshot --debug                   # save raw frames as frame_N.png
./target/release/scrollshot --scroll-delay 300        # slower settle (default 200ms)
./target/release/scrollshot --scroll-ticks 3          # more scroll per step (default 2)
```

No tests or linter configured.

## Maintenance

Keep `README.md` in sync with any changes to CLI flags, usage examples, or behaviour. This includes the usage code block, the options table, and the workflow/debug sections.

## Releasing a New Version

1. Update the version in `Cargo.toml` (e.g. `version = "0.1.2"`)
2. Run `cargo build --release` to update `Cargo.lock`
3. Commit both files: `git add Cargo.toml Cargo.lock && git commit -m "chore: bump version to X.Y.Z"`
4. Push the commit: `git push origin main`
5. Create and push a tag — **the tag must start with `v`** or the CI pipeline will not trigger:
   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

If a tag needs to be moved to a new commit (e.g. version was forgotten before tagging):
```bash
git tag -f vX.Y.Z
git push origin vX.Y.Z --force
```

## Architecture

Wayland scrolling screenshot tool for wlroots-based compositors. Three-stage pipeline:

1. **selector.rs** — Layer-shell overlay for click-drag region selection. Uses mmap'd SHM buffer with incremental damage tracking for efficient rendering. Returns a `Rect`.

2. **screencopy.rs** — Capture loop. Scrolls via `zwlr_virtual_pointer` (configurable ticks/delay via CLI), captures frames via `zwlr_screencopy_manager`. Stops after 2 consecutive unchanged frames. Returns `Vec<RgbaImage>`.

3. **stitch.rs** — Merges overlapping frames. Two-phase algorithm:
   - **Scroll detection**: For every row in prev frame, find best-matching row in next (sampled SAD), vote on implied offset. Content rows overwhelm ambiguous blank rows.
   - **Seam finding**: Within overlap, cut at the row where both frames are most pixel-similar (full-width SAD, middle 80% of overlap).
   - Slices are assembled: frame 1 up to `scroll+seam`, each subsequent frame from its seam row to the next frame's `scroll+seam`, last frame to bottom.

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

These files can then be re-stitched offline with `scrollshot -i .`.

Use `--capture-only <DIR>` to save frames to a specific directory without stitching, then stitch separately with `scrollshot -i <DIR> --cleanup`.
