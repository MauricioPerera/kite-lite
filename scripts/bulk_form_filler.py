"""Bulk form filler: reads a CSV of data rows and calls a page's
declarative WebMCP tool (via browser_call_tool) once per row,
re-navigating to the form page before each submission since a
successful call navigates away to the result page. Useful for loading
test data in bulk against a form that already declares itself as a
WebMCP tool (see webmcp_lint / the "Soporte declarativo de WebMCP"
section in the README).

Usage:
    python scripts/bulk_form_filler.py <url> <tool_name> <data.csv>

The CSV header row must match the tool's field names (the `name`
attributes on the form's inputs).

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import csv
import json
import os
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))

_id = 0


def start_mcp():
    return subprocess.Popen(
        [BINARY, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, bufsize=1,
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
        raise RuntimeError(f"mcp server closed stdout. stderr: {proc.stderr.read()}")
    resp = json.loads(line)
    if "error" in resp:
        raise RuntimeError(resp["error"])
    return resp["result"]


def call_tool(proc, name, arguments):
    result = send(proc, "tools/call", {"name": name, "arguments": arguments})
    text = "".join(item.get("text", "") for item in result.get("content", []))
    return result.get("isError", False), text


def main():
    if len(sys.argv) != 4:
        print("usage: bulk_form_filler.py <url> <tool_name> <data.csv>", file=sys.stderr)
        sys.exit(2)
    url, tool_name, csv_path = sys.argv[1], sys.argv[2], sys.argv[3]

    with open(csv_path, newline="", encoding="utf-8") as f:
        rows = list(csv.DictReader(f))
    if not rows:
        print("el CSV no tiene filas de datos", file=sys.stderr)
        sys.exit(2)

    proc = start_mcp()
    ok_count = 0
    try:
        send(proc, "initialize", {})
        for i, row in enumerate(rows):
            nav_error, nav_text = call_tool(proc, "browser_navigate", {"url": url})
            if nav_error:
                print(f"[{i}] ERROR navegando a {url}: {nav_text}")
                continue
            call_error, call_text = call_tool(proc, "browser_call_tool", {"name": tool_name, "arguments": row})
            if call_error:
                print(f"[{i}] FALLO {row} -> {call_text}")
                continue
            page = json.loads(call_text)
            ok_count += 1
            print(f"[{i}] OK {row} -> titulo={page.get('title')!r} texto={page.get('text', '')[:100]!r}")
    finally:
        proc.terminate()

    print(f"\n{ok_count}/{len(rows)} fila(s) enviadas correctamente")
    if ok_count != len(rows):
        sys.exit(1)


if __name__ == "__main__":
    main()
