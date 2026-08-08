"""Cheap monitoring fleet: watches N sites in parallel using kite-lite's
one-shot CLI fetch (no server/session kept running) — each check is a
genuinely separate ~4-9MB process (see README's "Recursos minimos"), so
watching a large number of sites doesn't cost what a real browser pool
would.

Usage:
    python scripts/fleet_monitor.py <state.json> <url1> <url2> ... [--concurrency N]

Each run fetches every URL concurrently, fingerprints its title+text, and
diffs against the previous run's fingerprint (persisted in <state.json>).
Prints NUEVO/CAMBIO/SIN CAMBIOS per site plus total wall time.

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import concurrent.futures
import hashlib
import json
import sys
import time
from pathlib import Path

from _kite_lite import FetchError, fetch_page


def fetch_one(url):
    try:
        page = fetch_page(url)
    except FetchError as error:
        return url, None, str(error)
    fingerprint_source = (page.get("title") or "") + "\n" + (page.get("text") or "")
    digest = hashlib.sha256(fingerprint_source.encode("utf-8")).hexdigest()
    return url, {"title": page.get("title"), "hash": digest}, None


def main():
    if len(sys.argv) < 3:
        print("usage: fleet_monitor.py <state.json> <url1> [url2 ...] [--concurrency N]", file=sys.stderr)
        sys.exit(2)
    state_path = Path(sys.argv[1])
    args = sys.argv[2:]
    concurrency = 8
    if "--concurrency" in args:
        idx = args.index("--concurrency")
        concurrency = int(args[idx + 1])
        del args[idx : idx + 2]
    urls = args

    state = json.loads(state_path.read_text()) if state_path.exists() else {}

    start = time.time()
    results = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
        for url, info, error in pool.map(fetch_one, urls):
            results[url] = (info, error)
    elapsed = time.time() - start

    changed = new = unchanged = failed = 0
    for url in urls:
        info, error = results[url]
        if error:
            print(f"[ERROR] {url}: {error}")
            failed += 1
            continue
        previous = state.get(url)
        if previous is None:
            print(f"[NUEVO] {url}: \"{info['title']}\"")
            new += 1
        elif previous.get("hash") != info["hash"]:
            print(f"[CAMBIO] {url}: \"{previous.get('title')}\" -> \"{info['title']}\"")
            changed += 1
        else:
            print(f"[SIN CAMBIOS] {url}")
            unchanged += 1
        state[url] = info

    state_path.write_text(json.dumps(state, indent=2, ensure_ascii=False))
    print(
        f"\n{len(urls)} sitio(s) revisados en {elapsed:.2f}s con concurrencia={concurrency} "
        f"-> nuevos={new} cambios={changed} sin_cambios={unchanged} errores={failed}"
    )


if __name__ == "__main__":
    main()
