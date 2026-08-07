"""Site changelog: keeps a full history (not just the last state, unlike
fleet_monitor.py) of a page's content and produces a human-readable
changelog of what changed and when. Diffs at the DOM block-element level
(paragraphs/headings/list items) rather than kite-lite's flattened
page.text, since that has no paragraph breaks left to diff meaningfully
line-by-line.

Usage:
    python scripts/site_changelog.py <url> <history.jsonl> [--changelog-out CHANGELOG.md]

Each run: if the page's blocks differ from the last recorded snapshot,
appends a new entry to history.jsonl and a dated diff section to the
changelog (stdout, or appended to --changelog-out). If unchanged, does
nothing but report that.

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import difflib
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))
BLOCK_TAGS = {"p", "li", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote", "td", "th"}


def fetch_page(url):
    proc = subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"exit code {proc.returncode}")
    return json.loads(proc.stdout)


def collect_block_texts(element, out):
    if element.get("tag") in BLOCK_TAGS:
        text = " ".join(element.get("text", "").split())
        if text:
            out.append(text)
        return
    for child in element.get("children", []):
        collect_block_texts(child, out)


def load_history(path):
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def append_history(path, entry):
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(entry, ensure_ascii=False) + "\n")


def render_diff_section(timestamp, url, title, old_blocks, new_blocks):
    lines = [f"## {timestamp} — {title or url}", ""]
    diff = list(difflib.unified_diff(old_blocks, new_blocks, lineterm="", n=0))
    for line in diff:
        if line.startswith(("---", "+++", "@@")):
            continue
        lines.append(line)
    lines.append("")
    return "\n".join(lines)


def main():
    args = sys.argv[1:]
    positional, changelog_out = [], None
    i = 0
    while i < len(args):
        if args[i] == "--changelog-out":
            changelog_out = args[i + 1]
            i += 2
        else:
            positional.append(args[i])
            i += 1

    if len(positional) != 2:
        print("usage: site_changelog.py <url> <history.jsonl> [--changelog-out CHANGELOG.md]", file=sys.stderr)
        sys.exit(2)
    url, history_path = positional[0], Path(positional[1])

    page = fetch_page(url)
    blocks = []
    collect_block_texts(page["root"], blocks)
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC")

    history = load_history(history_path)

    if not history:
        append_history(history_path, {"timestamp": timestamp, "title": page.get("title"), "blocks": blocks})
        section = f"## {timestamp} — {page.get('title') or url}\n\n(primera observacion, {len(blocks)} bloque(s))\n"
        print(section)
        if changelog_out:
            Path(changelog_out).write_text(section + "\n", encoding="utf-8")
        return

    last = history[-1]
    if last["blocks"] == blocks:
        print(f"sin cambios desde {last['timestamp']}")
        return

    section = render_diff_section(timestamp, url, page.get("title"), last["blocks"], blocks)
    append_history(history_path, {"timestamp": timestamp, "title": page.get("title"), "blocks": blocks})
    print(section)
    if changelog_out:
        with open(changelog_out, "a", encoding="utf-8") as f:
            f.write(section + "\n")


if __name__ == "__main__":
    main()
