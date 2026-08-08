"""Table extractor: walks kite-lite's parsed DOM for <table> elements and
converts one directly to CSV or JSON — no per-site scraper needed for
tabular data. A first row made entirely of <th> becomes the header
(CSV header row, or JSON object keys); otherwise rows come out as plain
lists.

Usage:
    python scripts/table_extract.py <url|page.json> [--list]
    python scripts/table_extract.py <url|page.json> [--table-index N] [--format csv|json] [--out path]

Limitations: colspan/rowspan are ignored (cells are taken as literally
present, no grid reconstruction) and a table nested inside another
table's cell is not extracted as a separate table — both are real-world
messiness out of scope for this demo.

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import csv
import io
import json
import sys
from pathlib import Path

from _kite_lite import fetch_page


def load_page(target):
    if target.startswith("http://") or target.startswith("https://"):
        return fetch_page(target)
    return json.loads(Path(target).read_text(encoding="utf-8"))


def find_tables(element, out):
    if element.get("tag") == "table":
        out.append(element)
        return  # don't descend: a table nested in a cell isn't extracted separately
    for child in element.get("children", []):
        find_tables(child, out)


def find_rows(element, out):
    if element.get("tag") == "tr":
        out.append(element)
        return
    for child in element.get("children", []):
        find_rows(child, out)


def extract_cells(tr_element):
    return [child.get("text", "") for child in tr_element.get("children", []) if child.get("tag") in ("td", "th")]


def is_header_row(tr_element):
    cells = [c for c in tr_element.get("children", []) if c.get("tag") in ("td", "th")]
    return bool(cells) and all(c.get("tag") == "th" for c in cells)


def main():
    args = sys.argv[1:]
    positional, table_index, fmt, out_path, list_only = [], 0, "csv", None, False
    i = 0
    while i < len(args):
        if args[i] == "--table-index":
            table_index = int(args[i + 1])
            i += 2
        elif args[i] == "--format":
            fmt = args[i + 1]
            i += 2
        elif args[i] == "--out":
            out_path = args[i + 1]
            i += 2
        elif args[i] == "--list":
            list_only = True
            i += 1
        else:
            positional.append(args[i])
            i += 1

    if len(positional) != 1:
        print("usage: table_extract.py <url|page.json> [--list] [--table-index N] [--format csv|json] [--out path]", file=sys.stderr)
        sys.exit(2)

    page = load_page(positional[0])
    tables = []
    find_tables(page["root"], tables)

    if not tables:
        print("no se encontraron <table> en la pagina", file=sys.stderr)
        sys.exit(1)

    if list_only:
        for idx, table in enumerate(tables):
            rows = []
            find_rows(table, rows)
            preview = extract_cells(rows[0]) if rows else []
            print(f"[{idx}] {len(rows)} fila(s) — primera fila: {preview}")
        return

    if table_index >= len(tables):
        print(f"solo hay {len(tables)} tabla(s) en la pagina, indice {table_index} fuera de rango", file=sys.stderr)
        sys.exit(1)

    rows = []
    find_rows(tables[table_index], rows)
    data = [extract_cells(r) for r in rows]

    headers = None
    if data and is_header_row(rows[0]):
        headers = data[0]
        data = data[1:]

    if fmt == "json":
        records = [dict(zip(headers, row)) for row in data] if headers else data
        output = json.dumps(records, indent=2, ensure_ascii=False)
    else:
        buf = io.StringIO()
        writer = csv.writer(buf)
        if headers:
            writer.writerow(headers)
        writer.writerows(data)
        output = buf.getvalue()

    if out_path:
        Path(out_path).write_text(output, encoding="utf-8")
        print(f"escrito en {out_path} ({len(data)} fila(s))", file=sys.stderr)
    else:
        print(output)


if __name__ == "__main__":
    main()
