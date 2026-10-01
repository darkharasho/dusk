#!/usr/bin/env python3
"""Generate Dusk's app icon.

The mark is a pale crescent with a gold sun sitting in its curve: Moonlight
and Sunshine, visibly two things, which is what the app is.

Drawn from geometry rather than traced, so every size is exact rather than a
resample of one master. Edges come from supersampling — draw at 8x and let
LANCZOS do the antialiasing — because PIL has no analytic antialiased fill
and a hard-edged 16px icon looks broken next to its neighbours.

    python3 scripts/make_icon.py

Writes the PNG set, icon.ico, and on macOS icon.icns into src-tauri/icons/.
"""

import os
import shutil
import subprocess
import sys

from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
ICONS = os.path.join(HERE, os.pardir, "src-tauri", "icons")

# Unit-square geometry, so the mark is resolution-independent. The bite
# circle and the sun share a centre: that is what makes the crescent's inner
# curve and the sun concentric rather than merely near each other.
CRESCENT = (0.469, 0.500, 0.359)   # cx, cy, r
BITE     = (0.625, 0.500, 0.3125)
SUN      = (0.625, 0.500, 0.164)

BACKDROP = (0x10, 0x13, 0x1B, 0xFF)
MOON     = (0xDC, 0xE3, 0xEE, 0xFF)
SUN_INK  = (0xE8, 0xB3, 0x3A, 0xFF)

# The art sits inside a rounded square with breathing room, the way platform
# icons do; filling the canvas edge to edge reads as a screenshot, not an icon.
INSET = 0.085
RADIUS = 0.2237  # Big Sur's corner radius as a fraction of the side.

SS = 8  # supersampling factor


def _disc(draw, size, circle, fill):
    cx, cy, r = circle
    draw.ellipse(
        [(cx - r) * size, (cy - r) * size, (cx + r) * size, (cy + r) * size],
        fill=fill,
    )


def mark(size, backdrop=True):
    """The icon at `size` px, supersampled."""
    big = size * SS
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    if backdrop:
        inset = INSET * big
        d.rounded_rectangle(
            [inset, inset, big - inset, big - inset],
            radius=RADIUS * big,
            fill=BACKDROP,
        )

    # The crescent is a disc with a disc taken out of it. Punched on its own
    # layer because the hole has to remove the moon without also removing the
    # backdrop underneath.
    layer = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    ld = ImageDraw.Draw(layer)
    scale = 1.0 - (INSET * 2) - 0.10
    offset = (1.0 - scale) / 2

    def placed(c):
        cx, cy, r = c
        return (cx * scale + offset, cy * scale + offset, r * scale)

    _disc(ld, big, placed(CRESCENT), MOON)
    cx, cy, r = placed(BITE)
    ld.ellipse(
        [(cx - r) * big, (cy - r) * big, (cx + r) * big, (cy + r) * big],
        fill=(0, 0, 0, 0),
    )
    img.alpha_composite(layer)

    _disc(ImageDraw.Draw(img), big, placed(SUN), SUN_INK)

    return img.resize((size, size), Image.LANCZOS)


def main():
    os.makedirs(ICONS, exist_ok=True)

    # Tauri's expected set, plus the sizes Windows and the web want.
    for name, size in [
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("256x256.png", 256),
        ("512x512.png", 512),
        ("icon.png", 1024),
    ]:
        mark(size).save(os.path.join(ICONS, name))
        print("wrote", name)

    # .ico carries its own sizes; Windows picks per context.
    mark(256).save(
        os.path.join(ICONS, "icon.ico"),
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    print("wrote icon.ico")

    if sys.platform == "darwin" and shutil.which("iconutil"):
        iconset = os.path.join(ICONS, "icon.iconset")
        shutil.rmtree(iconset, ignore_errors=True)
        os.makedirs(iconset)
        for base in (16, 32, 128, 256, 512):
            mark(base).save(os.path.join(iconset, f"icon_{base}x{base}.png"))
            mark(base * 2).save(os.path.join(iconset, f"icon_{base}x{base}@2x.png"))
        subprocess.run(
            ["iconutil", "-c", "icns", iconset, "-o", os.path.join(ICONS, "icon.icns")],
            check=True,
        )
        shutil.rmtree(iconset)
        print("wrote icon.icns")
    else:
        # Not fatal: only macOS bundles need it, and only macOS can make it.
        print("skipped icon.icns (needs macOS and iconutil)")


if __name__ == "__main__":
    main()
