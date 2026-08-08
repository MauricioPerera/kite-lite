"""Page classifier: given a fixed list of categories, asks an Ollama
model to classify each URL by its real content (JSON mode) — useful for
organizing a catalog or auditing a site's taxonomy. The model's answer
is validated against the given category list; anything outside it (a
hallucinated category, or genuinely ambiguous content) is reported as
SIN_CLASIFICAR rather than silently trusted.

Usage:
    python scripts/page_classifier.py --categories "Cat1,Cat2,Cat3" <url1> [url2 ...]

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
UNCLASSIFIED = "SIN_CLASIFICAR"


def fetch_page(url):
    proc = subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"exit code {proc.returncode}")
    return json.loads(proc.stdout)


def classify(title, text, categories):
    options = ", ".join(categories)
    prompt = (
        f"Titulo: {title}\nContenido: {text[:1500]}\n\n"
        f"Clasifica esta pagina en EXACTAMENTE una de estas categorias: {options}. "
        f"Si de verdad no encaja en ninguna, usa '{UNCLASSIFIED}'.\n"
        'Responde en JSON con este formato exacto: {"category": "...", "reason": "..."}'
    )
    body = json.dumps(
        {"model": MODEL, "messages": [{"role": "user", "content": prompt}], "format": "json", "stream": False}
    ).encode()
    req = urllib.request.Request(OLLAMA_URL, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as resp:
        data = json.loads(resp.read())
    result = json.loads(data["message"]["content"])
    if result.get("category") not in categories:
        result["category"] = UNCLASSIFIED
    return result


def main():
    args = sys.argv[1:]
    categories, urls = None, []
    i = 0
    while i < len(args):
        if args[i] == "--categories":
            categories = [c.strip() for c in args[i + 1].split(",")]
            i += 2
        else:
            urls.append(args[i])
            i += 1

    if not categories or not urls:
        print('usage: page_classifier.py --categories "Cat1,Cat2,Cat3" <url1> [url2 ...]', file=sys.stderr)
        sys.exit(2)

    counts = {c: 0 for c in categories}
    counts[UNCLASSIFIED] = 0

    for url in urls:
        page = fetch_page(url)
        result = classify(page.get("title") or url, page.get("text", ""), categories)
        counts[result["category"]] += 1
        print(f"[{result['category']}] {page.get('title')!r} ({url})")
        print(f"  motivo: {result.get('reason', '')}")

    print("\nresumen:")
    for category, count in counts.items():
        if count:
            print(f"  {category}: {count}")


if __name__ == "__main__":
    main()
