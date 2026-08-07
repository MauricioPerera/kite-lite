"""Lightweight WebMCP router: given a goal and a starting URL, explores a
site (same-origin links only) through kite-lite's MCP server, using an LLM
as a one-decision-per-page classifier — call a discovered WebMCP tool,
follow specific links, or give up — instead of an open-ended multi-turn
browsing agent. Bounded by --max-pages so it can't wander indefinitely.

Usage:
    python scripts/webmcp_router.py "<goal>" <start_url> [--max-pages N]

Env overrides:
    KITE_LITE_BIN   path to the kite-lite binary (default: target/release build)
    OLLAMA_MODEL    model tag to use (default: gpt-oss:20b-cloud)
    OLLAMA_URL      chat endpoint (default: http://localhost:11434/api/chat)

Requires the ollama CLI already signed in to Ollama Cloud on this machine.
"""

import json
import os
import subprocess
import sys
import urllib.parse
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))
MODEL = os.environ.get("OLLAMA_MODEL", "gpt-oss:20b-cloud")
OLLAMA_URL = os.environ.get("OLLAMA_URL", "http://localhost:11434/api/chat")
MAX_LINKS_SHOWN = 20

_id = 0

META_TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "call_page_tool",
            "description": "Invoke one of the WebMCP tools discovered on the CURRENT page because it accomplishes the goal.",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Exact tool name from the page's discovered tools"},
                    "arguments": {"type": "object", "description": "Arguments matching that tool's inputSchema"},
                },
                "required": ["name"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "follow_links",
            "description": "None of the current page's tools fit the goal. Visit these links next, most promising first.",
            "parameters": {
                "type": "object",
                "properties": {
                    "urls": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "URLs to visit next, taken verbatim from the current page's links",
                    },
                },
                "required": ["urls"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "give_up",
            "description": "Nothing on this site looks like it will accomplish the goal.",
            "parameters": {
                "type": "object",
                "properties": {"reason": {"type": "string"}},
                "required": ["reason"],
            },
        },
    },
]


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
    resp = json.loads(line)
    if "error" in resp:
        raise RuntimeError(resp["error"])
    return resp["result"]


def call_tool(proc, name, arguments):
    result = send(proc, "tools/call", {"name": name, "arguments": arguments})
    text = "\n".join(item["text"] for item in result.get("content", []) if item.get("type") == "text")
    if result.get("isError"):
        raise RuntimeError(f"{name} failed: {text}")
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text


def ask_router(goal, page_summary):
    tools = page_summary.get("tools", []) if isinstance(page_summary, dict) else []
    links = (page_summary.get("links", []) if isinstance(page_summary, dict) else [])[:MAX_LINKS_SHOWN]
    prompt = (
        f"Goal: {goal}\n\n"
        f"Current page: {page_summary.get('title')} ({page_summary.get('url')})\n\n"
        f"WebMCP tools discovered on this page:\n"
        f"{json.dumps(tools, indent=2) if tools else '(none)'}\n\n"
        "Links on this page (follow at most one or two, most relevant first):\n"
        + "\n".join(f"- {link}" for link in links)
        + "\n\n"
        "Decide: call_page_tool if one of the tools above accomplishes the goal, "
        "follow_links to explore further if not, or give_up if this site clearly won't have it."
    )
    body = json.dumps(
        {"model": MODEL, "messages": [{"role": "user", "content": prompt}], "tools": META_TOOLS, "stream": False}
    ).encode()
    req = urllib.request.Request(OLLAMA_URL, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as resp:
        data = json.loads(resp.read())
    tool_calls = data["message"].get("tool_calls") or []
    if not tool_calls:
        # Some models occasionally write the tool call as JSON in plain
        # content instead of using native tool-calling. Try to recover it
        # before falling back to treating the raw content as a give_up reason.
        content = data["message"].get("content", "")
        parsed = parse_inline_tool_call(content)
        if parsed is not None:
            return parsed
        return {"action": "give_up", "reason": content or "no decision returned"}
    call = tool_calls[0]["function"]
    args = call.get("arguments", {})
    if isinstance(args, str):
        args = json.loads(args) if args else {}
    return {"action": call["name"], **args}


def parse_inline_tool_call(content):
    try:
        parsed = json.loads(content)
    except (json.JSONDecodeError, TypeError):
        return None
    name = parsed.get("name")
    if name not in {"call_page_tool", "follow_links", "give_up"}:
        return None
    args = parsed.get("arguments") or {}
    if isinstance(args, str):
        try:
            args = json.loads(args) if args else {}
        except json.JSONDecodeError:
            args = {}
    return {"action": name, **args}


def same_origin(url_a, url_b):
    a, b = urllib.parse.urlparse(url_a), urllib.parse.urlparse(url_b)
    return (a.scheme, a.netloc) == (b.scheme, b.netloc)


def main():
    if len(sys.argv) < 3:
        print('usage: webmcp_router.py "<goal>" <start_url> [--max-pages N]', file=sys.stderr)
        sys.exit(2)
    goal = sys.argv[1]
    start_url = sys.argv[2]
    max_pages = 5
    if "--max-pages" in sys.argv:
        max_pages = int(sys.argv[sys.argv.index("--max-pages") + 1])

    proc = start_mcp()
    try:
        send(proc, "initialize", {})
        visited = set()
        queue = [start_url]
        pages_visited = 0

        while queue and pages_visited < max_pages:
            url = queue.pop(0)
            if url in visited:
                continue
            visited.add(url)
            pages_visited += 1
            print(f"[{pages_visited}/{max_pages}] navegando a {url}", file=sys.stderr)
            page = call_tool(proc, "browser_navigate", {"url": url})
            decision = ask_router(goal, page)
            action = decision.get("action")
            print(f"  -> decision: {decision}", file=sys.stderr)

            if action == "call_page_tool":
                tool_name = decision.get("name")
                args = decision.get("arguments") or {}
                result = call_tool(proc, "browser_call_tool", {"name": tool_name, "arguments": args})
                print("\n=== TOOL LLAMADA ===")
                print(f"tool: {tool_name}")
                print(f"arguments: {args}")
                print("\n=== RESULTADO ===")
                print(json.dumps(result, indent=2, ensure_ascii=False))
                return

            if action == "give_up":
                print(f"\n=== SIN MATCH ===\n{decision.get('reason', '(sin razon)')}")
                return

            if action == "follow_links":
                candidates = decision.get("urls") or []
                page_links = set(page.get("links", [])) if isinstance(page, dict) else set()
                for link in candidates:
                    if link in page_links and link not in visited and same_origin(link, start_url):
                        queue.append(link)
                continue

            print(f"decision desconocida, se ignora: {decision}", file=sys.stderr)

        print(
            f"\n=== AGOTADO ===\nSe visitaron {pages_visited} pagina(s) sin encontrar "
            f"una tool que cumpla el objetivo: {goal}"
        )
    finally:
        proc.terminate()


if __name__ == "__main__":
    main()
