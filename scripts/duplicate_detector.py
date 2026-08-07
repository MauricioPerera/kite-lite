"""Duplicate content detector: fetches several pages via kite-lite and
finds near-identical ones using shingling + Jaccard similarity (the
classic near-duplicate-detection technique, simplified) — a fuzzy
measure that tolerates small edits, unlike fleet_monitor.py's exact
content-hash comparison. Useful for SEO (duplicate content across
pages) or catching a template that isn't actually varying its content.

Usage:
    python scripts/duplicate_detector.py <url1> <url2> [url3 ...] [--threshold 0.7] [--shingle-size 5]

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import json
import os
import subprocess
import sys
from itertools import combinations
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))


def fetch_text(url):
    proc = subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"exit code {proc.returncode}")
    page = json.loads(proc.stdout)
    return page.get("title"), page.get("text", "")


def shingles(text, size):
    words = text.split()
    if len(words) < size:
        return {tuple(words)} if words else set()
    return {tuple(words[i : i + size]) for i in range(len(words) - size + 1)}


def jaccard(a, b):
    if not a and not b:
        return 1.0
    if not a or not b:
        return 0.0
    return len(a & b) / len(a | b)


def main():
    args = sys.argv[1:]
    urls, threshold, shingle_size = [], 0.7, 5
    i = 0
    while i < len(args):
        if args[i] == "--threshold":
            threshold = float(args[i + 1])
            i += 2
        elif args[i] == "--shingle-size":
            shingle_size = int(args[i + 1])
            i += 2
        else:
            urls.append(args[i])
            i += 1

    if len(urls) < 2:
        print("usage: duplicate_detector.py <url1> <url2> [url3 ...] [--threshold 0.7] [--shingle-size 5]", file=sys.stderr)
        sys.exit(2)

    pages = {}
    for url in urls:
        print(f"fetching {url}", file=sys.stderr)
        title, text = fetch_text(url)
        pages[url] = {"title": title, "shingles": shingles(text, shingle_size)}

    flagged = []
    for url_a, url_b in combinations(urls, 2):
        score = jaccard(pages[url_a]["shingles"], pages[url_b]["shingles"])
        if score >= threshold:
            flagged.append((score, url_a, url_b))

    flagged.sort(reverse=True)
    if not flagged:
        print(f"\nSin pares por encima del umbral ({threshold:.0%}). Ninguna pagina parece duplicada.")
        return

    print(f"\n{len(flagged)} par(es) de contenido posiblemente duplicado (umbral {threshold:.0%}):\n")
    for score, url_a, url_b in flagged:
        print(f"[{score:.1%}] {pages[url_a]['title']!r} ({url_a})")
        print(f"        ~ {pages[url_b]['title']!r} ({url_b})")
    sys.exit(1)


if __name__ == "__main__":
    main()
