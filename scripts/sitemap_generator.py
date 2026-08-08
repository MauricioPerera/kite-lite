"""Sitemap / site map generator: crawls same-origin pages from a start
URL (bounded by --max-pages) and produces a standard sitemap.xml plus a
readable indented tree of how pages link to each other. Each page is
attached under the FIRST page that linked to it during the breadth-
first crawl, so the tree stays a tree even if the real site is a graph
with multiple paths to the same page.

Usage:
    python scripts/sitemap_generator.py <start_url> [--max-pages N] [--out sitemap.xml]

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import json
import os
import subprocess
import sys
import urllib.parse
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))


def fetch(url):
    try:
        return subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=20)
    except subprocess.TimeoutExpired:
        return subprocess.CompletedProcess(args=[BINARY, "fetch", url], returncode=124, stdout="", stderr="timeout")


def same_origin(a, b):
    pa, pb = urllib.parse.urlparse(a), urllib.parse.urlparse(b)
    return (pa.scheme, pa.netloc) == (pb.scheme, pb.netloc)


def crawl(start_url, max_pages):
    visited = {}
    queue = [(start_url, None)]
    order = []
    pages_crawled = 0
    while queue and pages_crawled < max_pages:
        url, parent = queue.pop(0)
        if url in visited:
            continue
        proc = fetch(url)
        pages_crawled += 1
        if proc.returncode != 0:
            print(f"[ERROR] {url}: {proc.stderr.strip()}", file=sys.stderr)
            continue
        page = json.loads(proc.stdout)
        visited[url] = {"title": page.get("title"), "parent": parent, "children": []}
        order.append(url)
        if parent is not None and parent in visited:
            visited[parent]["children"].append(url)
        for link in page.get("links", []):
            if same_origin(link, start_url) and link not in visited:
                queue.append((link, url))
    return visited, order


def render_tree(visited, url, depth=0):
    node = visited[url]
    lines = [f"{'  ' * depth}- {node['title'] or url} ({url})"]
    for child in node["children"]:
        lines.extend(render_tree(visited, child, depth + 1))
    return lines


def render_sitemap_xml(urls):
    items = "\n".join(f"  <url><loc>{url}</loc></url>" for url in urls)
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
        f"{items}\n"
        "</urlset>\n"
    )


def main():
    args = sys.argv[1:]
    if not args:
        print("usage: sitemap_generator.py <start_url> [--max-pages N] [--out sitemap.xml]", file=sys.stderr)
        sys.exit(2)
    start_url = args[0]
    max_pages = 20
    out_path = None
    if "--max-pages" in args:
        max_pages = int(args[args.index("--max-pages") + 1])
    if "--out" in args:
        out_path = args[args.index("--out") + 1]

    print(f"crawleando desde {start_url} (max {max_pages} pagina(s))...", file=sys.stderr)
    visited, order = crawl(start_url, max_pages)

    print(f"\n{len(order)} pagina(s) encontradas:\n")
    print("\n".join(render_tree(visited, start_url)))

    xml = render_sitemap_xml(order)
    if out_path:
        Path(out_path).write_text(xml, encoding="utf-8")
        print(f"\nsitemap.xml escrito en {out_path} ({len(order)} url(s))", file=sys.stderr)
    else:
        print(f"\n{xml}")


if __name__ == "__main__":
    main()
