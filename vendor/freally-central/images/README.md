# Images

## `freally_app_icon.png` — the canonical Freally Central icon (1254×1254, RGBA)

Single source of truth for **both** the desktop app's icon **and** the docs site's
logo/favicon. If the icon ever changes, replace this file and regenerate the derivatives
below — never hand-edit them.

### Docs site (generated in Phase 0)

Derivatives live in `docs/` and are referenced by every page's `<head>` + the header logo:

| File | Size | Source | Use |
|------|------|--------|-----|
| `docs/freally-icon.png` | 512×512 | padded | Header logo (shown at 96px, with rounded-corner + shadow CSS) |
| `docs/favicon-32/48/96/192.png` | 32–192 | full-bleed | Browser-tab favicons; browser picks the sharpest for its DPI |
| `docs/favicon.ico` | 16→256 | full-bleed | Classic `/favicon.ico` fallback |
| `docs/apple-touch-icon.png` | 180×180 | full-bleed | iOS home-screen icon |

The **logo keeps the icon's transparent margin** (so the CSS `border-radius` + `box-shadow`
frame it nicely). The **favicons trim that margin** (crop to the opaque badge, re-centered on
a square) so the emblem fills the tiny frame and stays legible, with a light unsharp mask on
the small sizes. Regenerate with Pillow:

```python
from PIL import Image, ImageFilter
src = Image.open('images/freally_app_icon.png').convert('RGBA')

# Logo — padded, high quality
src.resize((512, 512), Image.LANCZOS).save('docs/freally-icon.png', optimize=True)

# Full-bleed square (drop transparent margins) for the favicons + apple-touch
bbox = src.split()[3].getbbox()
c = src.crop(bbox); side = max(c.size)
square = Image.new('RGBA', (side, side), (0, 0, 0, 0))
square.paste(c, ((side - c.size[0]) // 2, (side - c.size[1]) // 2))

for s in (32, 48, 96, 192):
    im = square.resize((s, s), Image.LANCZOS).filter(
        ImageFilter.UnsharpMask(radius=0.8, percent=90, threshold=0))
    im.save(f'docs/favicon-{s}.png', optimize=True)
square.resize((180, 180), Image.LANCZOS).save('docs/apple-touch-icon.png', optimize=True)
square.save('docs/favicon.ico', format='ICO',
            sizes=[(16,16),(32,32),(48,48),(64,64),(128,128),(256,256)])
```

### Desktop app (Phase 1 — FC-01)

Generate the full Tauri icon set from this file and wire it as the app icon. From the Tauri
app directory:

```
npx tauri icon ../images/freally_app_icon.png
```

That writes `src-tauri/icons/` (`32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.icns`,
`icon.ico`, and the Windows `Square*Logo` / `StoreLogo` PNGs), which `tauri.conf.json`
references under `bundle.icon`. Commit the generated set; regenerate from this source rather
than editing individual sizes so they never drift.
