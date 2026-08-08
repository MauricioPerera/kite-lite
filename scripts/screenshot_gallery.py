"""Screenshot gallery: renders each given URL to PNG via kite-lite and
builds a single self-contained HTML page (base64-embedded images, no
external files) showing them all in a grid — useful for reviewing
several pages/sites visually at once.

Usage:
    python scripts/screenshot_gallery.py <url1> [url2 ...] [--out gallery.html]

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import base64
import sys
from pathlib import Path

from _kite_lite import FetchError, fetch_page, render_png


def fetch_title(url):
    try:
        return fetch_page(url).get("title")
    except FetchError:
        return None


def escape(text):
    return (text or "").replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def build_html(entries):
    cards = []
    for title, url, png_bytes, error in entries:
        if error:
            cards.append(f'<div class="card"><div class="caption error">{escape(url)}<br>ERROR: {escape(error)}</div></div>')
            continue
        b64 = base64.b64encode(png_bytes).decode("ascii")
        cards.append(
            f'<div class="card">'
            f'<img src="data:image/png;base64,{b64}" alt="{escape(title or url)}">'
            f'<div class="caption"><strong>{escape(title) or "(sin titulo)"}</strong><br>'
            f'<a href="{escape(url)}">{escape(url)}</a></div>'
            f"</div>"
        )
    return (
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>Galeria de screenshots</title>\n"
        "<style>\n"
        "body { font-family: sans-serif; background: #111; color: #eee; margin: 0; padding: 20px; }\n"
        ".grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 16px; }\n"
        ".card { background: #1c1c1c; border-radius: 8px; overflow: hidden; }\n"
        ".card img { width: 100%; display: block; border-bottom: 1px solid #333; }\n"
        ".caption { padding: 10px; font-size: 14px; }\n"
        ".caption a { color: #8ab4f8; }\n"
        ".error { color: #f28b82; padding: 20px; }\n"
        "</style></head>\n"
        f"<body><h1>Galeria de screenshots ({len(entries)})</h1><div class=\"grid\">\n"
        + "\n".join(cards)
        + "\n</div></body></html>"
    )


def main():
    args = sys.argv[1:]
    urls, out_path = [], "gallery.html"
    i = 0
    while i < len(args):
        if args[i] == "--out":
            out_path = args[i + 1]
            i += 2
        else:
            urls.append(args[i])
            i += 1

    if not urls:
        print("usage: screenshot_gallery.py <url1> [url2 ...] [--out gallery.html]", file=sys.stderr)
        sys.exit(2)

    entries = []
    for url in urls:
        print(f"rendering {url}", file=sys.stderr)
        title = None
        try:
            title = fetch_title(url)
            png_bytes = render_png(url)
            entries.append((title, url, png_bytes, None))
        except Exception as error:
            entries.append((title, url, None, str(error)))

    html = build_html(entries)
    Path(out_path).write_text(html, encoding="utf-8")
    ok = sum(1 for entry in entries if entry[3] is None)
    print(f"galeria escrita en {out_path} ({ok}/{len(urls)} captura(s) OK)")
    if ok != len(urls):
        sys.exit(1)


if __name__ == "__main__":
    main()
