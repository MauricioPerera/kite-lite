"""Event/date extractor: scans a page's DOM block texts for dates and
times using regex (ISO, DD/MM/YYYY, Spanish "15 de marzo de 2026", and
HH:MM times) and reports each match with the surrounding text as
context — useful for building an agenda/schedule from a page that
mentions events informally, without needing an LLM.

Honest limitation: this is pattern matching, not a real date parser —
it finds things that LOOK like dates/times and reports them with
context for a human (or a downstream LLM) to interpret; it doesn't
resolve "next Friday" or validate that "31/02/2026" is a real date.

Usage:
    python scripts/event_extractor.py <url|page.json> [--json]

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import json
import re
import sys
from pathlib import Path

from _kite_lite import fetch_page

BLOCK_TAGS = {"p", "li", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote", "td", "th"}

MONTHS_ES = (
    "enero|febrero|marzo|abril|mayo|junio|julio|agosto|"
    "septiembre|setiembre|octubre|noviembre|diciembre"
)
PATTERNS = [
    ("fecha_iso", re.compile(r"\b\d{4}-\d{2}-\d{2}\b")),
    ("fecha_barra", re.compile(r"\b\d{1,2}/\d{1,2}/\d{2,4}\b")),
    ("fecha_texto", re.compile(rf"\b\d{{1,2}}\s+de\s+(?:{MONTHS_ES})(?:\s+de\s+\d{{4}})?\b", re.IGNORECASE)),
    ("hora", re.compile(r"\b\d{1,2}:\d{2}\s?(?:hs\.?|am|pm)?\b", re.IGNORECASE)),
]
CONTEXT_RADIUS = 40


def load_page(target):
    if target.startswith("http://") or target.startswith("https://"):
        return fetch_page(target)
    return json.loads(Path(target).read_text(encoding="utf-8"))


def collect_block_texts(element, out):
    if element.get("tag") in BLOCK_TAGS:
        text = " ".join(element.get("text", "").split())
        if text:
            out.append(text)
        return
    for child in element.get("children", []):
        collect_block_texts(child, out)


def extract_events(blocks):
    events = []
    for block in blocks:
        for kind, pattern in PATTERNS:
            for match in pattern.finditer(block):
                start = max(0, match.start() - CONTEXT_RADIUS)
                end = min(len(block), match.end() + CONTEXT_RADIUS)
                context = ("..." if start > 0 else "") + block[start:end] + ("..." if end < len(block) else "")
                events.append({"type": kind, "text": match.group(), "context": context})
    return events


def main():
    args = sys.argv[1:]
    targets = [a for a in args if a != "--json"]
    as_json = "--json" in args
    if len(targets) != 1:
        print("usage: event_extractor.py <url|page.json> [--json]", file=sys.stderr)
        sys.exit(2)

    page = load_page(targets[0])
    blocks = []
    collect_block_texts(page["root"], blocks)
    events = extract_events(blocks)

    if as_json:
        print(json.dumps(events, indent=2, ensure_ascii=False))
        return

    if not events:
        print("No se encontraron fechas ni horarios.")
        return
    for event in events:
        print(f"[{event['type']}] {event['text']}")
        print(f"  contexto: {event['context']}")


if __name__ == "__main__":
    main()
