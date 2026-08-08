"""Broken link auditor: crawls same-origin pages from a starting URL (up
to --max-pages) to build the full set of outbound links, then checks
every one concurrently — each check is a cheap, independent kite-lite
process — classifying it as OK, REDIRECT (final URL differs from the
linked one), BROKEN (4xx/5xx), or ERROR (DNS/connection/timeout failure).

Usage:
    python scripts/link_checker.py <start_url> [--max-pages N] [--concurrency N]

Exits non-zero if any BROKEN or ERROR link was found.

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import concurrent.futures
import json
import os
import re
import subprocess
import sys
import urllib.parse
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))
STATUS_RE = re.compile(r"\((\d{3})\b")


def fetch(url):
    try:
        return subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=20)
    except subprocess.TimeoutExpired:
        return subprocess.CompletedProcess(args=[BINARY, "fetch", url], returncode=124, stdout="", stderr="timeout")


def same_origin(a, b):
    pa, pb = urllib.parse.urlparse(a), urllib.parse.urlparse(b)
    return (pa.scheme, pa.netloc) == (pb.scheme, pb.netloc)


def crawl(start_url, max_pages):
    visited = set()
    queue = [start_url]
    all_links = set()
    pages_crawled = 0
    while queue and pages_crawled < max_pages:
        url = queue.pop(0)
        if url in visited:
            continue
        visited.add(url)
        proc = fetch(url)
        pages_crawled += 1
        if proc.returncode != 0:
            print(f"[CRAWL ERROR] {url}: {proc.stderr.strip()}", file=sys.stderr)
            continue
        page = json.loads(proc.stdout)
        for link in page.get("links", []):
            all_links.add(link)
            if same_origin(link, start_url) and link not in visited:
                queue.append(link)
    return all_links


def check_link(url):
    proc = fetch(url)
    if proc.returncode == 0:
        page = json.loads(proc.stdout)
        final_url = page.get("url") or url
        if final_url != url:
            return url, "REDIRECT", f"-> {final_url}"
        return url, "OK", ""
    stderr = proc.stderr.strip()
    match = STATUS_RE.search(stderr)
    if match:
        return url, "BROKEN", f"HTTP {match.group(1)}"
    return url, "ERROR", stderr.splitlines()[-1] if stderr else "unknown error"


def main():
    args = sys.argv[1:]
    if not args:
        print("usage: link_checker.py <start_url> [--max-pages N] [--concurrency N]", file=sys.stderr)
        sys.exit(2)
    start_url = args[0]
    max_pages = 1
    concurrency = 8
    if "--max-pages" in args:
        max_pages = int(args[args.index("--max-pages") + 1])
    if "--concurrency" in args:
        concurrency = int(args[args.index("--concurrency") + 1])

    print(f"crawleando desde {start_url} (max {max_pages} pagina(s))...", file=sys.stderr)
    links = crawl(start_url, max_pages)
    print(f"{len(links)} link(s) unico(s) encontrados, revisando...", file=sys.stderr)

    results = {"OK": [], "REDIRECT": [], "BROKEN": [], "ERROR": []}
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
        for url, status, detail in pool.map(check_link, sorted(links)):
            results[status].append((url, detail))
            print(f"[{status}] {url} {detail}")

    print(
        f"\nresumen: {len(results['OK'])} OK, {len(results['REDIRECT'])} redirect, "
        f"{len(results['BROKEN'])} roto(s), {len(results['ERROR'])} error(es)"
    )
    if results["BROKEN"] or results["ERROR"]:
        sys.exit(1)


if __name__ == "__main__":
    main()
