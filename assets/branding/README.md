# ph0nix phoenix

Original pixel artwork for the emulator's app branding. The phoenix has raised
flame-shaped wings, a right-facing head and a flowing ember tail. Its five flat
colors and transparent background work on both light and dark surfaces.

- `phoenix.svg`: scalable mark with a 64 × 64 pixel grid.
- `phoenix.png`: transparent 512 × 512 PNG, enlarged eight times without smoothing.
- `phoenix-icon.png`: transparent 64 × 64 PNG for window icons and favicons.
- `phoenix.pixels.json`: editable pixel grid and palette; `.` is transparent.

After editing the grid, run `python3 scripts/generate_branding.py` from the
project root. The script exports the canonical assets, browser copies and
Android vector drawables together using only the Python standard library.
Android launcher resources include legacy, adaptive and monochrome icons.
The desktop embeds the icon and uses nearest-neighbor filtering in its header.
