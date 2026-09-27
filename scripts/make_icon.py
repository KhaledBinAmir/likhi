"""Render assets/icons/Likhi_icon.svg to multi-resolution Windows .ico icon.

Generates icon frames for 16, 20, 24, 32, 48, 64, 128, and 256 pixels.
Matches Windows standard ICO structure:
  - 16..48 px: uncompressed 32bpp BGRA DIB with BITMAPINFOHEADER (expected by GDI LoadImageW)
  - 64..256 px: PNG compressed
"""

from __future__ import annotations

import io
import math
import os
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path
from PIL import Image

REPO = Path(__file__).resolve().parents[1]
SVG_PATH = REPO / "assets" / "icons" / "Likhi_icon.svg"
SIZES = [16, 20, 24, 32, 48, 64, 128, 256]


def find_edge() -> str:
    candidates = [
        Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "Microsoft" / "Edge" / "Application" / "msedge.exe",
        Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "Microsoft" / "Edge" / "Application" / "msedge.exe",
        Path(os.environ.get("LOCALAPPDATA", "")) / "Microsoft" / "Edge" / "Application" / "msedge.exe",
    ]
    for p in candidates:
        if p.exists():
            return str(p)
    found = shutil.which("msedge")
    if found:
        return found
    raise RuntimeError("Microsoft Edge not found for headless SVG rendering")


def render_svg_to_png(svg_path: Path, out_png: Path, size: int = 1024) -> None:
    svg_content = svg_path.read_text(encoding="utf-8")
    html_template = """<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
* {{ margin: 0; padding: 0; box-sizing: border-box; }}
html, body {{
  background: rgba(0, 0, 0, 0);
  width: {size}px;
  height: {size}px;
  overflow: hidden;
}}
svg {{
  width: {size}px;
  height: {size}px;
  display: block;
}}
</style>
</head>
<body>
{svg}
</body>
</html>"""
    html = html_template.format(size=size, svg=svg_content)
    with tempfile.TemporaryDirectory() as td:
        html_file = Path(td) / "icon.html"
        html_file.write_text(html, encoding="utf-8")
        edge = find_edge()
        cmd = [
            edge,
            "--headless=new",
            "--disable-gpu",
            "--force-device-scale-factor=1",
            "--default-background-color=00000000",
            f"--window-size={size},{size}",
            f"--screenshot={out_png}",
            html_file.as_uri(),
        ]
        subprocess.run(cmd, check=True, capture_output=True)


def get_dib_bytes(img: Image.Image) -> bytes:
    """32bpp DIB frame: BITMAPINFOHEADER + bottom-up BGRA rows + AND mask."""
    w, h = img.size
    # 1-bit AND mask row padded to 4 bytes boundary
    mask_row = math.floor((w + 31) / 32) * 4
    mask_size = mask_row * h
    image_size = w * h * 4 + mask_size

    header = struct.pack(
        "<IiiHHIIiiII",
        40,          # biSize
        w,           # biWidth
        h * 2,       # biHeight (doubled height in ICO format)
        1,           # biPlanes
        32,          # biBitCount
        0,           # biCompression (BI_RGB)
        image_size,  # biSizeImage
        0,           # biXPelsPerMeter
        0,           # biYPelsPerMeter
        0,           # biClrUsed
        0,           # biClrImportant
    )

    # Bottom-up BGRA rows
    rgba_data = img.tobytes("raw", "RGBA")
    row_len = w * 4
    bgra_rows = bytearray()
    for y in range(h - 1, -1, -1):
        row = rgba_data[y * row_len : (y + 1) * row_len]
        # convert RGBA to BGRA
        for x in range(0, len(row), 4):
            r = row[x]
            g = row[x + 1]
            b = row[x + 2]
            a = row[x + 3]
            bgra_rows.extend([b, g, r, a])

    and_mask = b"\x00" * mask_size
    return header + bytes(bgra_rows) + and_mask


def get_png_bytes(img: Image.Image) -> bytes:
    buf = io.BytesIO()
    img.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def build_ico(master_img: Image.Image, out_ico: Path) -> None:
    entries = []
    for s in SIZES:
        resized = master_img.resize((s, s), Image.Resampling.LANCZOS)
        if s >= 64:
            payload = get_png_bytes(resized)
        else:
            payload = get_dib_bytes(resized)
        entries.append((s, payload))

    # Calculate offsets
    # Header: 6 bytes (idReserved, idType, idCount)
    # Entry: 16 bytes each
    offset = 6 + 16 * len(entries)
    header = struct.pack("<HHH", 0, 1, len(entries))

    entry_headers = bytearray()
    payload_data = bytearray()

    for s, data in entries:
        dim = 0 if s == 256 else s
        entry_headers.extend(struct.pack(
            "<BBBBHHII",
            dim,         # bWidth
            dim,         # bHeight
            0,           # bColorCount
            0,           # bReserved
            1,           # wPlanes
            32,          # wBitCount
            len(data),   # dwBytesInRes
            offset,      # dwImageOffset
        ))
        offset += len(data)
        payload_data.extend(data)

    out_ico.parent.mkdir(parents=True, exist_ok=True)
    with open(out_ico, "wb") as f:
        f.write(header)
        f.write(entry_headers)
        f.write(payload_data)

    print(f"[icon] generated {out_ico} ({out_ico.stat().st_size} bytes)")


def main() -> None:
    if not SVG_PATH.exists():
        raise SystemExit(f"SVG icon not found at {SVG_PATH}")

    with tempfile.TemporaryDirectory() as td:
        master_png = Path(td) / "master.png"
        print(f"[icon] rendering {SVG_PATH} at 1024x1024...")
        render_svg_to_png(SVG_PATH, master_png, size=1024)
        master_img = Image.open(master_png).convert("RGBA")

        # Destination paths to update
        dest_paths = [
            REPO / "windows" / "pime" / "likhi" / "icon.ico",
            REPO / "assets" / "icons" / "Likhi_icon.ico",
            REPO / "assets" / "icons" / "likhi.ico",
        ]

        # If dist has shell directories, update them too
        for arch in ["x64", "x86", "arm64"]:
            shell_dir = REPO / "dist" / "shell" / arch
            if shell_dir.exists():
                dest_paths.append(shell_dir / "likhi.ico")

        for dest in dest_paths:
            build_ico(master_img, dest)


if __name__ == "__main__":
    main()
