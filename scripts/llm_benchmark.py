"""Deterministic benchmark: compares how well different LLMs use kite-lite's
MCP tools on the SAME fixed local pages (no ads, no anti-bot, no
variability run to run — the site never changes), with a hardcoded,
non-LLM-judged grading function per task. Reproducible pass/fail, not
vibes.

Usage:
    python scripts/llm_benchmark.py [--models m1,m2,m3] [--max-turns N]

Env override: KITE_LITE_BIN (path to the kite-lite binary), OLLAMA_URL.
Requires the ollama CLI already signed in to Ollama Cloud on this machine.
"""

import json
import os
import subprocess
import sys
import threading
import time
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))
OLLAMA_URL = os.environ.get("OLLAMA_URL", "http://localhost:11434/api/chat")
DEFAULT_MODELS = ["gpt-oss:20b-cloud", "qwen3.5:cloud", "deepseek-v4-flash:cloud"]
SECRET_PHRASE = "PALABRA-CLAVE-XYZ123"

_id = 0


# --- fixed mock site: same content every run, no external network -----

class SiteHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        route, query = parsed.path, parsed.query
        base = f"http://127.0.0.1:{self.server.server_address[1]}"
        if route == "/":
            body = (
                "<title>Tienda Acme</title>"
                f'<a href="{base}/directory">Directorio</a> '
                f'<a href="{base}/support">Soporte</a>'
                f'<form action="{base}/search"><input name="q"><button type="submit">Buscar</button></form>'
            )
        elif route == "/directory":
            body = f'<title>Directorio</title><a href="{base}/deep/contact">Mas info</a>'
        elif route == "/deep/contact":
            body = f"<title>Contacto secreto</title>La palabra clave es: {SECRET_PHRASE}"
        elif route == "/support":
            body = (
                "<title>Soporte Acme</title>"
                f'<form toolname="submit-ticket" tooldescription="Enviar un ticket de soporte" action="{base}/submit-ticket">'
                '<input type="text" name="issue" toolparamdescription="Descripcion del problema">'
                '<button type="submit">Enviar</button></form>'
            )
        elif route == "/search":
            q = urllib.parse.parse_qs(query).get("q", [""])[0]
            body = f"<title>Resultados</title>Resultados para: {q}"
        elif route == "/submit-ticket":
            issue = urllib.parse.parse_qs(query).get("issue", [""])[0]
            body = f"<title>Ticket creado</title>Ticket creado: {issue}"
        else:
            self.send_response(404)
            self.end_headers()
            return
        encoded = body.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


def start_site():
    server = ThreadingHTTPServer(("127.0.0.1", 0), SiteHandler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, f"http://127.0.0.1:{server.server_address[1]}"


# --- kite-lite MCP bridge (same pattern as scripts/mcp_ollama_bridge.py) --

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


def mcp_tools_to_ollama(tools):
    return [
        {"type": "function", "function": {"name": t["name"], "description": t.get("description", ""), "parameters": t["inputSchema"]}}
        for t in tools
    ]


def call_ollama(model, messages, tools):
    body = json.dumps({"model": model, "messages": messages, "tools": tools, "stream": False}).encode()
    req = urllib.request.Request(OLLAMA_URL, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as resp:
        return json.loads(resp.read())


def run_task(model, prompt, max_turns):
    """Runs one (model, task) pair against a fresh kite-lite mcp process.
    Returns {"tool_calls": [(name, args, result_text)], "final_text": str}."""
    proc = start_mcp()
    try:
        send(proc, "initialize", {})
        tools = mcp_tools_to_ollama(send(proc, "tools/list", {})["tools"])
        messages = [{"role": "user", "content": prompt}]
        tool_calls = []
        for _ in range(max_turns):
            resp = call_ollama(model, messages, tools)
            msg = resp["message"]
            messages.append(msg)
            calls = msg.get("tool_calls")
            if not calls:
                return {"tool_calls": tool_calls, "final_text": msg.get("content", "")}
            for tc in calls:
                fn = tc["function"]
                name = fn["name"]
                args = fn.get("arguments", {})
                if isinstance(args, str):
                    args = json.loads(args) if args else {}
                result = send(proc, "tools/call", {"name": name, "arguments": args})
                text = "\n".join(i.get("text", "") for i in result.get("content", []))
                tool_calls.append((name, args, text))
                messages.append({"role": "tool", "name": name, "content": text})
        return {"tool_calls": tool_calls, "final_text": "(max_turns agotado)"}
    finally:
        proc.terminate()


# --- tasks: fixed prompt + deterministic, non-LLM-judged grading ---------

def grade_fetch_title(result):
    haystack = result["final_text"] + " ".join(t for _, _, t in result["tool_calls"])
    return "Tienda Acme" in haystack


def grade_form_submit(result):
    haystack = result["final_text"] + " ".join(t for _, _, t in result["tool_calls"])
    return "zapatillas" in haystack and "Resultados" in haystack


def grade_webmcp_tool(result):
    used_webmcp = any(name == "browser_call_tool" for name, _, _ in result["tool_calls"])
    haystack = result["final_text"] + " ".join(t for _, _, t in result["tool_calls"])
    accomplished = "Ticket creado" in haystack
    return accomplished, used_webmcp


def grade_link_following(result):
    haystack = result["final_text"] + " ".join(t for _, _, t in result["tool_calls"])
    return SECRET_PHRASE in haystack


TASKS = [
    {
        "key": "fetch_title",
        "prompt": lambda base: f"Usa fetch_page para traer {base}/ y decime cual es el titulo de la pagina.",
        "grade": lambda r: (grade_fetch_title(r), None),
    },
    {
        "key": "form_submit",
        "prompt": lambda base: (
            f"Navega a {base}/ con browser_navigate y usa el formulario de busqueda de la pagina "
            "(hace click y escribi, no uses ninguna herramienta de WebMCP) para buscar 'zapatillas'. "
            "Decime que dice la pagina de resultados."
        ),
        "grade": lambda r: (grade_form_submit(r), None),
    },
    {
        "key": "webmcp_tool",
        "prompt": lambda base: (
            f"Navega a {base}/support y reporta que 'mi pedido llego incompleto' "
            "usando la herramienta que la pagina te ofrezca."
        ),
        "grade": lambda r: grade_webmcp_tool(r),
    },
    {
        "key": "link_following",
        "prompt": lambda base: (
            f"Navega a {base}/ y encontra la pagina secreta de contacto siguiendo los links "
            "(no esta en la home). Decime la palabra clave exacta que encuentres ahi."
        ),
        "grade": lambda r: (grade_link_following(r), None),
    },
]


def main():
    models = DEFAULT_MODELS
    max_turns = 8
    args = sys.argv[1:]
    if "--models" in args:
        models = args[args.index("--models") + 1].split(",")
    if "--max-turns" in args:
        max_turns = int(args[args.index("--max-turns") + 1])

    _, base = start_site()
    print(f"sitio fijo de prueba en {base}", file=sys.stderr)

    results = {}
    for model in models:
        results[model] = {}
        for task in TASKS:
            prompt = task["prompt"](base)
            start = time.time()
            try:
                run = run_task(model, prompt, max_turns)
                ok, extra = task["grade"](run)
            except Exception as error:  # noqa: BLE001 - benchmark keeps going on any model/task failure
                ok, extra, run = False, None, {"tool_calls": [], "final_text": f"ERROR: {error}"}
            elapsed = time.time() - start
            results[model][task["key"]] = {
                "pass": ok,
                "extra": extra,
                "turns": len(run["tool_calls"]),
                "seconds": elapsed,
            }
            note = f" (webmcp={extra})" if extra is not None else ""
            print(
                f"[{model}] {task['key']}: {'PASS' if ok else 'FAIL'} "
                f"({len(run['tool_calls'])} tool-calls, {elapsed:.1f}s){note}",
                file=sys.stderr,
            )

    print("\n=== RESULTADOS ===")
    header = "modelo".ljust(24) + "".join(t["key"].ljust(16) for t in TASKS) + "score"
    print(header)
    for model in models:
        row = model.ljust(24)
        score = 0
        for task in TASKS:
            cell = results[model][task["key"]]
            score += cell["pass"]
            row += ("PASS" if cell["pass"] else "FAIL").ljust(16)
        row += f"{score}/{len(TASKS)}"
        print(row)


if __name__ == "__main__":
    main()
