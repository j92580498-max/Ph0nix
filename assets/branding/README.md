# Ph0nix button-phone logo

Original pixel artwork on a 32 × 32 grid: a dark handset with a silver rim,
green LCD, navigation pad and twelve physical buttons. The background is
transparent and the colors are flat.

- `logo.svg`: scalable pixel mark with crisp edges.
- `logo.png`: transparent 512 × 512 PNG, enlarged without smoothing.
- `logo-icon.png`: transparent 64 × 64 window icon and favicon.
- `logo.pixels.json`: editable source grid and palette.

Run `python3 scripts/generate_branding.py` to export the source artwork to the
browser and Android vector resources. Adaptive launcher assets leave room for
the icon mask; the monochrome version retains screen and keypad cutouts.
Desktop textures use nearest-neighbor filtering.
