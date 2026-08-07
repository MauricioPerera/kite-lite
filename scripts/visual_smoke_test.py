"""CI-friendly visual/structural smoke test: navigates to a page via
kite-lite's MCP server and checks title/text/selectors, plus a real
pixel-level "not blank" check on the rendered PNG using a small stdlib-only
PNG decoder (no Pillow). That specific regression class — a PNG that's
structurally valid but silently all-white — is exactly the fontconfig bug
this project hit once (see README's "Renderizado PNG/PDF"); this script
exists to catch it (or anything like it) as a CI gate instead of by eye.

Usage:
    python scripts/visual_smoke_test.py <url> <checks.json>

checks.json (all keys optional):
{
  "title_contains": "Example",
  "text_contains": ["some phrase", "another phrase"],
  "selectors_exist": ["h1", "a"],
  "min_png_bytes": 500,
  "check_not_blank": true,
  "min_nonwhite_fraction": 0.001
}

Exits non-zero if any check fails.

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import base64
import json
import os
import struct
import subprocess
import sys
import zlib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))

_id = 0


def start_mcp():
    return subprocess.Popen(
        [BINARY, "mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )


def send(proc, method, params=None):
    global _id
    _id += 1
    req = {"jsonrpc": "2.0", "id": _id, "method": method}
    if params is not None:
        req["params"] = params
    proc.stdin.write(json.dumps(req) + "\n")
    proc.stdin.flush()
    line = proc.stdout.readline()
    if not line:
        raise RuntimeError(f"mcp server closed stdout unexpectedly. stderr: {proc.stderr.read()}")
    return json.loads(line)


def call_tool(proc, name, arguments):
    resp = send(proc, "tools/call", {"name": name, "arguments": arguments})
    result = resp["result"]
    return result.get("isError", False), result.get("content", [])


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


def nonwhite_fraction(width, height, channels, pixels):
    total = width * height
    if total == 0:
        return 0.0
    nonwhite = 0
    for i in range(0, len(pixels), channels):
        if pixels[i : i + 3] != b"\xff\xff\xff":
            nonwhite += 1
    return nonwhite / total


def main():
    if len(sys.argv) != 3:
        print("usage: visual_smoke_test.py <url> <checks.json>", file=sys.stderr)
        sys.exit(2)
    url = sys.argv[1]
    checks = json.loads(Path(sys.argv[2]).read_text())

    failures = []
    proc = start_mcp()
    try:
        send(proc, "initialize", {})

        is_error, content = call_tool(proc, "browser_navigate", {"url": url})
        if is_error:
            print(f"FAIL: browser_navigate error: {content}", file=sys.stderr)
            sys.exit(1)
        page = json.loads(content[0]["text"])

        title_contains = checks.get("title_contains")
        if title_contains is not None:
            ok = title_contains in (page.get("title") or "")
            print(f"{'PASS' if ok else 'FAIL'}: title contains '{title_contains}' (title='{page.get('title')}')")
            if not ok:
                failures.append("title_contains")

        for phrase in checks.get("text_contains", []):
            ok = phrase in (page.get("text") or "")
            print(f"{'PASS' if ok else 'FAIL'}: text contains '{phrase}'")
            if not ok:
                failures.append(f"text_contains:{phrase}")

        for selector in checks.get("selectors_exist", []):
            sel_error, _ = call_tool(proc, "browser_get_dom", {"selector": selector})
            ok = not sel_error
            print(f"{'PASS' if ok else 'FAIL'}: selector '{selector}' exists")
            if not ok:
                failures.append(f"selector_exists:{selector}")

        min_png_bytes = checks.get("min_png_bytes", 200)
        check_blank = checks.get("check_not_blank", True)
        if min_png_bytes or check_blank:
            png_error, png_content = call_tool(proc, "browser_screenshot", {"format": "png"})
            if png_error:
                print(f"FAIL: browser_screenshot error: {png_content}")
                failures.append("render_png")
            else:
                png_bytes = base64.b64decode(png_content[0]["data"])
                ok = len(png_bytes) >= min_png_bytes
                print(f"{'PASS' if ok else 'FAIL'}: PNG size {len(png_bytes)} >= {min_png_bytes} bytes")
                if not ok:
                    failures.append("min_png_bytes")

                if check_blank:
                    width, height, channels, pixels = decode_png(png_bytes)
                    fraction = nonwhite_fraction(width, height, channels, pixels)
                    threshold = checks.get("min_nonwhite_fraction", 0.001)
                    ok = fraction >= threshold
                    print(
                        f"{'PASS' if ok else 'FAIL'}: PNG not blank "
                        f"({fraction:.4%} non-white pixels, need >= {threshold:.4%})"
                    )
                    if not ok:
                        failures.append("check_not_blank")
    finally:
        proc.terminate()

    if failures:
        print(f"\n{len(failures)} check(s) failed: {failures}")
        sys.exit(1)
    print("\nTodos los checks pasaron.")


if __name__ == "__main__":
    main()
