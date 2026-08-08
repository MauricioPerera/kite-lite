"""FAQ/summary generator: extracts a page's real content via kite-lite
(at the DOM block level, same approach as rag_ingest.py), then asks an
Ollama model (JSON mode) for an executive summary and a FAQ grounded
only in that content — explicitly instructed not to invent facts.

Usage:
    python scripts/faq_generator.py <url> [--num-questions N] [--out out.md]

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


def generate(title, blocks, num_questions):
    prompt = (
        f"Este es el contenido real de una pagina titulada '{title}':\n\n"
        + "\n".join(f"- {b}" for b in blocks)
        + "\n\nGenera:\n"
        "1. Un resumen ejecutivo de 2-3 oraciones.\n"
        f"2. Una lista de {num_questions} preguntas frecuentes (FAQ) con sus respuestas, "
        "basadas UNICAMENTE en la informacion de arriba. No inventes datos, precios, "
        "plazos ni numeros que no esten explicitamente en el texto.\n\n"
        'Responde en JSON con este formato exacto: {"summary": "...", '
        '"faq": [{"question": "...", "answer": "..."}]}'
    )
    body = json.dumps(
        {"model": MODEL, "messages": [{"role": "user", "content": prompt}], "format": "json", "stream": False}
    ).encode()
    req = urllib.request.Request(OLLAMA_URL, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as resp:
        data = json.loads(resp.read())
    return parse_json_response(data["message"]["content"])


def render_markdown(title, url, result):
    lines = [f"# {title}", "", f"_Fuente: {url}_", "", "## Resumen", "", result.get("summary", ""), "", "## FAQ", ""]
    for item in result.get("faq", []):
        lines.append(f"**{item.get('question', '')}**")
        lines.append("")
        lines.append(item.get("answer", ""))
        lines.append("")
    return "\n".join(lines)


def main():
    args = sys.argv[1:]
    if not args:
        print("usage: faq_generator.py <url> [--num-questions N] [--out out.md]", file=sys.stderr)
        sys.exit(2)
    url = args[0]
    num_questions = 5
    out_path = None
    if "--num-questions" in args:
        num_questions = int(args[args.index("--num-questions") + 1])
    if "--out" in args:
        out_path = args[args.index("--out") + 1]

    print(f"fetching {url}", file=sys.stderr)
    page = fetch_page(url)
    blocks = []
    collect_block_texts(page["root"], blocks)
    print(f"{len(blocks)} bloque(s) de contenido, generando con {MODEL}...", file=sys.stderr)

    result = generate(page.get("title") or url, blocks, num_questions)
    markdown = render_markdown(page.get("title") or url, page.get("url") or url, result)

    if out_path:
        Path(out_path).write_text(markdown, encoding="utf-8")
        print(f"escrito en {out_path}", file=sys.stderr)
    else:
        print(markdown)


if __name__ == "__main__":
    main()
