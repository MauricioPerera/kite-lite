"""On-the-fly site translator: extracts a page's content at the DOM
block level (paragraphs/headings/list items) and translates it with an
Ollama model, returning original and translated text side by side — no
real browser needed. Sends all blocks in one request and validates the
model returned exactly as many translations as blocks sent, so nothing
got silently merged or dropped.

Usage:
    python scripts/site_translator.py <url> [--to English] [--out out.md]

Env overrides:
    KITE_LITE_BIN   path to the kite-lite binary
    OLLAMA_URL      chat endpoint (default: http://localhost:11434/api/chat)
    OLLAMA_MODEL    model tag (default: gpt-oss:20b-cloud)
"""

import json
import os
import subprocess
import sys
import urllib.request
from pathlib import Path

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))
OLLAMA_URL = os.environ.get("OLLAMA_URL", "http://localhost:11434/api/chat")
MODEL = os.environ.get("OLLAMA_MODEL", "gpt-oss:20b-cloud")
BLOCK_TAGS = {"p", "li", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote", "td", "th"}


def parse_json_response(content):
    """Ollama's `format: "json"` doesn't guarantee fence-free output for
    every model — some (glm-5.2:cloud observed doing this) still wrap the
    JSON in a ```json ... ``` markdown block. Strip that before parsing."""
    stripped = content.strip()
    if stripped.startswith("```"):
        stripped = stripped.strip("`")
        if stripped.lower().startswith("json"):
            stripped = stripped[4:]
        stripped = stripped.strip()
    try:
        return json.loads(stripped)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"respuesta del modelo no es JSON valido: {content[:300]!r}") from error


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


def translate_blocks(blocks, target_language):
    numbered = "\n".join(f"{i}: {b}" for i, b in enumerate(blocks))
    prompt = (
        f"Traduce cada uno de estos {len(blocks)} fragmentos de texto al idioma '{target_language}'. "
        "Mantene el mismo orden y la MISMA CANTIDAD de fragmentos: no combines ni omitas ninguno. "
        "No agregues informacion ni comentarios, solo traduci el contenido tal cual esta.\n\n"
        f"{numbered}\n\n"
        f'Responde en JSON con este formato exacto: {{"translations": [array de {len(blocks)} strings]}}'
    )
    body = json.dumps(
        {"model": MODEL, "messages": [{"role": "user", "content": prompt}], "format": "json", "stream": False}
    ).encode()
    req = urllib.request.Request(OLLAMA_URL, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as resp:
        data = json.loads(resp.read())
    result = parse_json_response(data["message"]["content"])
    translations = result.get("translations", [])
    if len(translations) != len(blocks):
        raise RuntimeError(f"se esperaban {len(blocks)} traducciones, se recibieron {len(translations)}")
    return translations


def render(title, url, target_language, blocks, translations):
    lines = [f"# {title} -> {target_language}", "", f"_Fuente: {url}_", ""]
    for original, translated in zip(blocks, translations):
        lines.append(f"**Original:** {original}")
        lines.append(f"**Traduccion:** {translated}")
        lines.append("")
    return "\n".join(lines)


def main():
    args = sys.argv[1:]
    if not args:
        print("usage: site_translator.py <url> [--to English] [--out out.md]", file=sys.stderr)
        sys.exit(2)
    url = args[0]
    target_language = "English"
    out_path = None
    if "--to" in args:
        target_language = args[args.index("--to") + 1]
    if "--out" in args:
        out_path = args[args.index("--out") + 1]

    print(f"fetching {url}", file=sys.stderr)
    page = fetch_page(url)
    blocks = []
    collect_block_texts(page["root"], blocks)
    print(f"{len(blocks)} bloque(s), traduciendo a '{target_language}' con {MODEL}...", file=sys.stderr)

    translations = translate_blocks(blocks, target_language)
    output = render(page.get("title") or url, page.get("url") or url, target_language, blocks, translations)

    if out_path:
        Path(out_path).write_text(output, encoding="utf-8")
        print(f"escrito en {out_path}", file=sys.stderr)
    else:
        print(output)


if __name__ == "__main__":
    main()
