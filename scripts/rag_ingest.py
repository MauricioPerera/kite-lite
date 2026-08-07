"""Lightweight RAG ingestion pipeline: fetches a list of documentation
URLs via kite-lite, chunks each page at natural block-element boundaries
(paragraphs, headings, list items, table cells — not a blind character
window, since kite-lite's flattened page.text already has no paragraph
breaks left), embeds each chunk with a local Ollama embedding model, and
writes a JSONL index ready for a vector DB. Optionally runs an in-memory
top-K cosine-similarity search against a query, to prove the chunks are
actually retrievable, not just present.

Usage:
    python scripts/rag_ingest.py <url1> [url2 ...] --out index.jsonl [--query "..."] [--top-k 3]

Env overrides:
    KITE_LITE_BIN      path to the kite-lite binary
    OLLAMA_URL         base URL (default: http://localhost:11434)
    OLLAMA_EMBED_MODEL embedding model tag (default: nomic-embed-text)
"""

import json
import math
import os
import subprocess
import sys
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))
OLLAMA_URL = os.environ.get("OLLAMA_URL", "http://localhost:11434")
EMBED_MODEL = os.environ.get("OLLAMA_EMBED_MODEL", "nomic-embed-text")

MAX_CHUNK_CHARS = 800
CHUNK_OVERLAP = 100
MIN_CHUNK_CHARS = 20
BLOCK_TAGS = {"p", "li", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote", "td", "th"}


def fetch_page(url):
    proc = subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"exit code {proc.returncode}")
    return json.loads(proc.stdout)


def collect_block_texts(element, out):
    if element.get("tag") in BLOCK_TAGS:
        text = " ".join(element.get("text", "").split())
        if len(text) >= MIN_CHUNK_CHARS:
            out.append((element["tag"], text))
        return  # don't descend further: this element's text already folds in all descendant text
    for child in element.get("children", []):
        collect_block_texts(child, out)


def split_long(text, max_chars, overlap):
    if len(text) <= max_chars:
        return [text]
    chunks = []
    step = max(max_chars - overlap, 1)
    start = 0
    while start < len(text):
        end = min(start + max_chars, len(text))
        chunks.append(text[start:end].strip())
        if end == len(text):
            break
        start += step
    return [c for c in chunks if c]


def embed(text):
    body = json.dumps({"model": EMBED_MODEL, "prompt": text}).encode()
    req = urllib.request.Request(
        f"{OLLAMA_URL}/api/embeddings", data=body, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read())["embedding"]


def cosine(a, b):
    dot = sum(x * y for x, y in zip(a, b))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(x * x for x in b))
    return dot / (na * nb) if na and nb else 0.0


def main():
    args = sys.argv[1:]
    urls, out_path, query, top_k = [], "index.jsonl", None, 3
    i = 0
    while i < len(args):
        if args[i] == "--out":
            out_path = args[i + 1]
            i += 2
        elif args[i] == "--query":
            query = args[i + 1]
            i += 2
        elif args[i] == "--top-k":
            top_k = int(args[i + 1])
            i += 2
        else:
            urls.append(args[i])
            i += 1

    if not urls:
        print('usage: rag_ingest.py <url1> [url2 ...] --out index.jsonl [--query "..."] [--top-k 3]', file=sys.stderr)
        sys.exit(2)

    chunks = []
    for url in urls:
        print(f"fetching {url}", file=sys.stderr)
        page = fetch_page(url)
        blocks = []
        collect_block_texts(page["root"], blocks)
        for tag, text in blocks:
            for piece in split_long(text, MAX_CHUNK_CHARS, CHUNK_OVERLAP):
                chunks.append({"url": page.get("url") or url, "title": page.get("title"), "tag": tag, "text": piece})

    print(f"{len(chunks)} chunks extraidos de {len(urls)} pagina(s), generando embeddings...", file=sys.stderr)
    for idx, chunk in enumerate(chunks):
        chunk["id"] = idx
        chunk["embedding"] = embed(chunk["text"])

    with open(out_path, "w", encoding="utf-8") as f:
        for chunk in chunks:
            f.write(json.dumps(chunk, ensure_ascii=False) + "\n")
    print(f"indice escrito en {out_path} ({len(chunks)} chunks)")

    if query:
        q_embedding = embed(query)
        scored = sorted(chunks, key=lambda c: cosine(c["embedding"], q_embedding), reverse=True)
        print(f"\nTop {top_k} resultados para: {query!r}\n")
        for chunk in scored[:top_k]:
            score = cosine(chunk["embedding"], q_embedding)
            print(f"[{score:.3f}] ({chunk['tag']}, {chunk['url']}) {chunk['text'][:150]}")


if __name__ == "__main__":
    main()
