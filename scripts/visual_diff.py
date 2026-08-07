"""Visual diff: renders two URLs to PNG at the same fixed viewport width
via kite-lite (its --png shortcut always renders at 1024px) and compares
them pixel-by-pixel, producing a diff image that highlights changed
regions in red over a dimmed version of the "after" page — a lightweight
visual-regression check (staging vs prod, before vs after a deploy)
without a real browser or a paid screenshot-diff service. Uses the same
stdlib-only PNG decoder as visual_smoke_test.py, plus a minimal encoder
for the diff image (no Pillow).

Usage:
    python scripts/visual_diff.py <url_before> <url_after> [--out diff.png] [--threshold N]

Env override: KITE_LITE_BIN (path to the kite-lite binary).

Caveat if KITE_LITE_BIN points at scripts/kite-lite-docker: unlike the
stdio/MCP-based scripts, this one needs kite-lite to write a PNG to a
*file*. That file only lives inside the container's own ephemeral
filesystem unless the wrapper bind-mounts a shared directory (the
shipped wrapper deliberately doesn't, to keep the container's rootfs
isolated) — you'd need a variant with e.g. `-v /tmp:/tmp` for this
script specifically. This script pre-creates the temp file world-
writable so it works once such a bind mount is in place, since the
container runs as a non-root uid that otherwise can't write into a
root-owned file at a shared path.
"""

import binascii
import os
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))


def render_png(url):
    fd, path = tempfile.mkstemp(suffix=".png")
    os.close(fd)
    os.chmod(path, 0o666)
    try:
        proc = subprocess.run([BINARY, url, "--png", path], capture_output=True, text=True, timeout=30)
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr.strip() or f"exit code {proc.returncode}")
        return Path(path).read_bytes()
    finally:
        os.unlink(path)


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


def decode_png(data):
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG file")
    pos = 8
    width = height = bit_depth = color_type = None
    idat = bytearray()
    while pos < len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        chunk_type = data[pos + 4 : pos + 8]
        chunk_data = data[pos + 8 : pos + 8 + length]
        pos += 12 + length
        if chunk_type == b"IHDR":
            width, height, bit_depth, color_type = struct.unpack(">IIBB", chunk_data[:10])
        elif chunk_type == b"IDAT":
            idat += chunk_data
        elif chunk_type == b"IEND":
            break

    channels = {0: 1, 2: 3, 4: 2, 6: 4}.get(color_type)
    if channels is None or bit_depth != 8:
        raise ValueError(f"unsupported PNG: color_type={color_type} bit_depth={bit_depth}")

    raw = zlib.decompress(bytes(idat))
    stride = width * channels
    pixels = bytearray(height * stride)
    prev_row = bytearray(stride)
    offset = 0
    for y in range(height):
        filter_type = raw[offset]
        offset += 1
        row = bytearray(raw[offset : offset + stride])
        offset += stride
        for x in range(stride):
            a = row[x - channels] if x >= channels else 0
            b = prev_row[x]
            c = prev_row[x - channels] if x >= channels else 0
            if filter_type == 1:
                row[x] = (row[x] + a) & 0xFF
            elif filter_type == 2:
                row[x] = (row[x] + b) & 0xFF
            elif filter_type == 3:
                row[x] = (row[x] + (a + b) // 2) & 0xFF
            elif filter_type == 4:
                row[x] = (row[x] + paeth(a, b, c)) & 0xFF
        pixels[y * stride : (y + 1) * stride] = row
        prev_row = row
    return width, height, channels, bytes(pixels)


def encode_png_rgb(width, height, pixels_rgb):
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", binascii.crc32(tag + data))

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    stride = width * 3
    raw = bytearray()
    for y in range(height):
        raw.append(0)  # filter type: None
        raw += pixels_rgb[y * stride : (y + 1) * stride]
    idat = zlib.compress(bytes(raw), 9)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


def rgb_at(pixels, stride, channels, x, y):
    i = y * stride + x * channels
    return pixels[i], pixels[i + 1], pixels[i + 2]


def make_diff_image(width, height, c1, p1, c2, p2, threshold):
    stride1 = width * c1
    stride2 = width * c2
    out = bytearray(width * height * 3)
    changed = 0
    for y in range(height):
        for x in range(width):
            r1, g1, b1 = rgb_at(p1, stride1, c1, x, y)
            r2, g2, b2 = rgb_at(p2, stride2, c2, x, y)
            out_i = (y * width + x) * 3
            if abs(r1 - r2) + abs(g1 - g2) + abs(b1 - b2) > threshold:
                out[out_i : out_i + 3] = b"\xff\x00\x00"
                changed += 1
            else:
                gray = (r2 + g2 + b2) // 3
                dim = min(255, gray // 3 + 170)
                out[out_i : out_i + 3] = bytes([dim, dim, dim])
    return bytes(out), changed


def main():
    raw = sys.argv[1:]
    positional = []
    out_path = "diff.png"
    threshold = 30
    i = 0
    while i < len(raw):
        if raw[i] == "--out":
            out_path = raw[i + 1]
            i += 2
        elif raw[i] == "--threshold":
            threshold = int(raw[i + 1])
            i += 2
        else:
            positional.append(raw[i])
            i += 1

    if len(positional) != 2:
        print("usage: visual_diff.py <url_before> <url_after> [--out diff.png] [--threshold N]", file=sys.stderr)
        sys.exit(2)
    url_before, url_after = positional

    print(f"renderizando 'antes': {url_before}", file=sys.stderr)
    before_png = render_png(url_before)
    print(f"renderizando 'despues': {url_after}", file=sys.stderr)
    after_png = render_png(url_after)

    w1, h1, c1, p1 = decode_png(before_png)
    w2, h2, c2, p2 = decode_png(after_png)

    if w1 != w2:
        print(f"ADVERTENCIA: anchos distintos ({w1}px vs {w2}px) — no deberia pasar, kite-lite renderiza a un ancho fijo", file=sys.stderr)
    if h1 != h2:
        print(f"el alto del layout cambio: {h1}px -> {h2}px (se compara solo el area en comun)", file=sys.stderr)

    width = min(w1, w2)
    height = min(h1, h2)
    diff_pixels, changed = make_diff_image(width, height, c1, p1, c2, p2, threshold)
    Path(out_path).write_bytes(encode_png_rgb(width, height, diff_pixels))

    total = width * height
    fraction = changed / total if total else 0.0
    print(f"\n{changed}/{total} pixeles distintos ({fraction:.2%}) en el area comparada ({width}x{height})")
    print(f"diff escrito en {out_path} (rojo = cambio, gris = sin cambio)")


if __name__ == "__main__":
    main()
