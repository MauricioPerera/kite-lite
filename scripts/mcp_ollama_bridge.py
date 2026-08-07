"""Drive kite-lite's MCP server with a real LLM client (an Ollama Cloud model)
instead of hand-scripted JSON-RPC, to exercise the tool schemas end to end.

Usage:
    python scripts/mcp_ollama_bridge.py ["prompt for the model"]

Env overrides:
    KITE_LITE_BIN   path to the kite-lite binary (default: target/release build)
    OLLAMA_MODEL    model tag to use (default: gpt-oss:20b-cloud)
    OLLAMA_URL      chat endpoint (default: http://localhost:11434/api/chat)

Requires the ollama CLI already signed in to Ollama Cloud on this machine.
"""

import base64
import json
import os
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))
MODEL = os.environ.get("OLLAMA_MODEL", "gpt-oss:20b-cloud")
OLLAMA_URL = os.environ.get("OLLAMA_URL", "http://localhost:11434/api/chat")
IMAGE_DIR = tempfile.gettempdir()

_id = 0
_image_count = 0


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
        err = proc.stderr.read()
        raise RuntimeError(f"mcp server closed stdout unexpectedly. stderr: {err}")
    resp = json.loads(line)
    if "error" in resp:
        raise RuntimeError(resp["error"])
    return resp["result"]


def mcp_tools_to_ollama(tools):
    return [
        {
            "type": "function",
            "function": {
                "name": t["name"],
                "description": t.get("description", ""),
                "parameters": t["inputSchema"],
            },
        }
        for t in tools
    ]


def call_ollama(messages, tools):
    body = json.dumps(
        {"model": MODEL, "messages": messages, "tools": tools, "stream": False}
    ).encode()
    req = urllib.request.Request(
        OLLAMA_URL, data=body, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=180) as resp:
        return json.loads(resp.read())


def tool_result_to_text(result):
    global _image_count
    parts = []
    for item in result.get("content", []):
        if item.get("type") == "text":
            parts.append(item["text"])
        elif item.get("type") == "image":
            _image_count += 1
            ext = "png" if "png" in item.get("mimeType", "") else "bin"
            path = os.path.join(IMAGE_DIR, f"kite-lite-mcp-screenshot-{_image_count}.{ext}")
            raw = base64.b64decode(item["data"])
            with open(path, "wb") as f:
                f.write(raw)
            parts.append(
                f"[image saved to {path}, mimeType={item.get('mimeType')}, "
                f"{len(raw)} bytes]"
            )
            print(f"   saved image -> {path} ({len(raw)} bytes)", file=sys.stderr)
    text = "\n".join(parts) if parts else json.dumps(result)
    if result.get("isError"):
        text = f"ERROR: {text}"
    return text


def main():
    prompt = (
        sys.argv[1]
        if len(sys.argv) > 1
        else "Usa la herramienta fetch_page para traer https://example.com "
        "y decime el titulo de la pagina y un resumen breve del texto."
    )
    proc = start_mcp()
    try:
        init = send(proc, "initialize", {})
        print(f"== initialize: {init} ==", file=sys.stderr)
        tools_result = send(proc, "tools/list", {})
        tools = mcp_tools_to_ollama(tools_result["tools"])
        print(f"== {len(tools)} tools loaded: {[t['function']['name'] for t in tools]} ==", file=sys.stderr)

        messages = [{"role": "user", "content": prompt}]
        for turn in range(10):
            print(f"\n--- turn {turn} : calling {MODEL} ---", file=sys.stderr)
            resp = call_ollama(messages, tools)
            msg = resp["message"]
            messages.append(msg)
            tool_calls = msg.get("tool_calls")
            if not tool_calls:
                print("\n=== FINAL RESPONSE ===")
                print(msg.get("content", ""))
                return
            for tc in tool_calls:
                fn = tc["function"]
                name = fn["name"]
                args = fn.get("arguments", {})
                if isinstance(args, str):
                    args = json.loads(args) if args else {}
                print(f"-> tool call: {name}({args})", file=sys.stderr)
                result = send(proc, "tools/call", {"name": name, "arguments": args})
                text = tool_result_to_text(result)
                print(f"<- tool result ({len(text)} chars): {text[:300]}", file=sys.stderr)
                messages.append({"role": "tool", "name": name, "content": text})
        print("max turns reached without a final answer", file=sys.stderr)
    finally:
        proc.terminate()


if __name__ == "__main__":
    main()
