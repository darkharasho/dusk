#!/usr/bin/env python3
"""Generate Dusk's placeholder app icon.

A twilight gradient with the last of the sun sitting on the horizon — the same
sun/moon idea the device grid uses. Real artwork is M5 work; this exists so the
app has an icon to build against.

    python3 scripts/make_placeholder_icon.py
"""

import struct
import zlib
from pathlib import Path

SIZES = (32, 128, 256, 512)
OUT_DIR = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"

TOP = (0x10, 0x14, 0x1F)
BOTTOM = (0x2A, 0x35, 0x56)
SUN = (0xF2, 0xB4, 0x5C)


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def screen(base, tint, amount):
    """Add light without clipping to white, so the glow keeps its colour."""
    return tuple(
        round(c + (t - c) * amount) if t > c else c for c, t in zip(base, tint)
    )


def pixel(x, y, size):
    u = x / (size - 1)
    v = y / (size - 1)

    colour = lerp(TOP, BOTTOM, v**1.6)

    # The sun, low and mostly below the horizon line.
    cx, cy = 0.5, 0.86
    d = (((u - cx) * 1.0) ** 2 + ((v - cy) * 1.0) ** 2) ** 0.5
    if d < 0.40:
        glow = (1 - d / 0.40) ** 2.2
        colour = screen(colour, SUN, glow * 0.85)

    return colour


def png(size):
    raw = bytearray()
    for y in range(size):
        raw.append(0)  # no per-scanline filter
        for x in range(size):
            raw.extend(pixel(x, y, size))
            raw.append(0xFF)

    def chunk(tag, data):
        body = tag + data
        return (
            struct.pack(">I", len(data))
            + body
            + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
        )

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def main():
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for size in SIZES:
        (OUT_DIR / f"{size}x{size}.png").write_bytes(png(size))
    (OUT_DIR / "icon.png").write_bytes(png(512))
    (OUT_DIR / "128x128@2x.png").write_bytes(png(256))
    print(f"wrote placeholder icons to {OUT_DIR}")


if __name__ == "__main__":
    main()
