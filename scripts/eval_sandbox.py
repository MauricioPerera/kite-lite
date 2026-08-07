"""Safe eval sandbox demo: runs untrusted JS snippets (LLM-generated
extraction code, student submissions, ...) against a page via kite-lite's
isolated eval_js — a genuinely separate process (kite-lite-js) with no
fetch/XHR, no require/process, no filesystem, and a hard timeout — and
grades each one as PASS/FAIL, distinguishing "wrong answer" from "tried to
escape the sandbox and got blocked".

Usage:
    python scripts/eval_sandbox.py <url> <snippets.json>

snippets.json is a list of objects:
    {"name": str, "kind": "benign"|"malicious", "script": str, "expect": str}
    {"name": str, "kind": "malicious", "script": str, "expect_timeout": true}

For "benign"/"malicious" snippets with "expect": PASS if the returned value
equals expect. For "expect_timeout": PASS if kite-lite reports a timeout
(the classic DoS attempt — an infinite loop — being contained).

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

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
        [BINARY, "mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
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
        raise RuntimeError(f"mcp server closed stdout unexpectedly. stderr: {proc.stderr.read()}")
    return json.loads(line)


def eval_snippet(proc, url, script):
    resp = send(proc, "tools/call", {"name": "eval_js", "arguments": {"url": url, "script": script}})
    result = resp["result"]
    text = "".join(item.get("text", "") for item in result.get("content", []))
    return result.get("isError", False), text


def grade(snippet, is_error, output):
    name = snippet["name"]
    kind = snippet.get("kind", "benign")

    if snippet.get("expect_timeout"):
        ok = is_error and "exceeded" in output
        verdict = "PASS (sandbox contuvo el bucle infinito)" if ok else "FAIL (no se corto el bucle a tiempo!)"
        return ok, f"[{kind}] {name}: {verdict} -- salida: {output}"

    expect = snippet.get("expect")
    if is_error:
        # An error on a "malicious" attempt to reach a forbidden global is
        # also a valid containment outcome, not just an exact `expect` match.
        if kind == "malicious":
            return True, f"[{kind}] {name}: PASS (bloqueado con error) -- {output}"
        return False, f"[{kind}] {name}: FAIL (broke con error) -- {output}"

    ok = expect is None or output.strip() == str(expect)
    verdict = "PASS" if ok else f"FAIL (se esperaba '{expect}', se obtuvo '{output}')"
    return ok, f"[{kind}] {name}: {verdict}"


def main():
    if len(sys.argv) != 3:
        print("usage: eval_sandbox.py <url> <snippets.json>", file=sys.stderr)
        sys.exit(2)
    url = sys.argv[1]
    snippets = json.loads(Path(sys.argv[2]).read_text())

    proc = start_mcp()
    try:
        send(proc, "initialize", {})
        passed = 0
        for snippet in snippets:
            is_error, output = eval_snippet(proc, url, snippet["script"])
            ok, message = grade(snippet, is_error, output)
            passed += ok
            print(message)
        print(f"\n{passed}/{len(snippets)} PASS")
        sys.exit(0 if passed == len(snippets) else 1)
    finally:
        proc.terminate()


if __name__ == "__main__":
    main()
