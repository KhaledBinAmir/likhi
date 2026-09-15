"""Write a simple icon.ico (16x16 + 32x32, 32-bit BGRA) without any imaging library.

The icon is a rounded green square with a white "ল"-like stroke placeholder; it only needs to be
recognizable in the language bar until a designed icon exists.
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path


def _image(size: int) -> bytes:
    bg = (0x3A, 0x8C, 0x2E, 0xFF)  # B, G, R, A  (green)
    fg = (0xFF, 0xFF, 0xFF, 0xFF)
    px = bytearray()
    r = size // 5
    for y in range(size - 1, -1, -1):  # BMP rows are bottom-up
        for x in range(size):
            # rounded corners
            cx = min(x, size - 1 - x)
            cy = min(y, size - 1 - y)
            inside = not (cx < r and cy < r and (r - cx) ** 2 + (r - cy) ** 2 > r * r)
            # a thick diagonal stroke + a horizontal bar, vaguely a pen mark
            stroke = abs(x - y) <= max(1, size // 10) or (
                size // 2 - size // 12 <= y <= size // 2 + size // 12
                and size // 4 <= x <= 3 * size // 4
            )
            color = fg if (inside and stroke) else (bg if inside else (0, 0, 0, 0))
            px.extend(color)
    # AND mask (1 bit per pixel, rows padded to 32 bits); all zero = opaque handled by alpha
    row_bytes = ((size + 31) // 32) * 4
    mask = bytes(row_bytes * size)
    header = struct.pack(
        "<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, len(px) + len(mask), 0, 0, 0, 0
    )
    return header + bytes(px) + mask


def write_ico(path: Path, sizes=(16, 32)) -> None:
    images = [_image(s) for s in sizes]
    offset = 6 + 16 * len(images)
    out = bytearray(struct.pack("<HHH", 0, 1, len(images)))
    for s, img in zip(sizes, images, strict=True):
        out += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(img), offset)
        offset += len(img)
    for img in images:
        out += img
    path.write_bytes(bytes(out))


if __name__ == "__main__":
    target = (
        Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent / "likhi" / "icon.ico"
    )
    write_ico(target)
    print(f"wrote {target} ({target.stat().st_size} bytes)")
