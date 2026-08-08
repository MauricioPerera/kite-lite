"""Page archiver: a mini wayback machine. Each run captures a full
snapshot of a page — its structured JSON (title/text/links/DOM) and a
PNG screenshot — timestamped and appended to a per-URL archive folder,
building a browsable record of how the page looked over time. Useful
for compliance/legal record-keeping or just watching a page evolve.

Archives the structured Page JSON rather than raw HTML bytes (kite-lite
doesn't expose the raw source over its CLI) — arguably more useful for
this purpose anyway: readable, diffable, and free of unrelated
boilerplate/tracking scripts.

Usage:
    python scripts/page_archiver.py <url> <archive_dir>

Each run writes <archive_dir>/<url_slug>/<timestamp>.json and
<timestamp>.png, and appends to that folder's manifest.jsonl.

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))


def fetch_page(url):
    proc = subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"exit code {proc.returncode}")
    return json.loads(proc.stdout)


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


def url_slug(url):
    digest = hashlib.sha256(url.encode("utf-8")).hexdigest()[:12]
    readable = re.sub(r"[^a-zA-Z0-9]+", "-", url).strip("-")[:60]
    return f"{readable}-{digest}"


def content_hash(page):
    return hashlib.sha256(((page.get("title") or "") + "\n" + page.get("text", "")).encode("utf-8")).hexdigest()


def main():
    if len(sys.argv) != 3:
        print("usage: page_archiver.py <url> <archive_dir>", file=sys.stderr)
        sys.exit(2)
    url, archive_root = sys.argv[1], Path(sys.argv[2])

    folder = archive_root / url_slug(url)
    folder.mkdir(parents=True, exist_ok=True)
    manifest_path = folder / "manifest.jsonl"

    page = fetch_page(url)
    png_bytes = render_png(url)
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")

    (folder / f"{timestamp}.json").write_text(json.dumps(page, indent=2, ensure_ascii=False), encoding="utf-8")
    (folder / f"{timestamp}.png").write_bytes(png_bytes)

    new_hash = content_hash(page)
    previous_hash = None
    if manifest_path.exists():
        lines = [json.loads(line) for line in manifest_path.read_text(encoding="utf-8").splitlines() if line.strip()]
        if lines:
            previous_hash = lines[-1]["content_hash"]

    with open(manifest_path, "a", encoding="utf-8") as f:
        f.write(
            json.dumps(
                {
                    "timestamp": timestamp,
                    "url": page.get("url") or url,
                    "title": page.get("title"),
                    "content_hash": new_hash,
                    "png_bytes": len(png_bytes),
                },
                ensure_ascii=False,
            )
            + "\n"
        )

    total = sum(1 for _ in manifest_path.read_text(encoding="utf-8").splitlines() if _.strip())
    changed_note = ""
    if previous_hash is not None:
        changed_note = " (sin cambios desde la captura anterior)" if previous_hash == new_hash else " (contenido distinto a la captura anterior)"
    print(f"snapshot {timestamp} guardado en {folder}{changed_note}")
    print(f"total de capturas para esta URL: {total}")


if __name__ == "__main__":
    main()
