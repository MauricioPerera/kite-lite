"""Shared helpers for the scripts/ demos: resolve the kite-lite binary,
fetch a page as parsed JSON, render a PNG, and clean up markdown-fenced
JSON that some Ollama models emit even in JSON mode. Each demo script
used to carry its own copy of this; consolidated here so a fix (timeout
handling, JSON-decode errors, fence stripping) lands everywhere at once.

Not a public API — import path relies on the script's own directory
being on sys.path, which Python does automatically for `python scripts/foo.py`.
"""

import json
import os
import subprocess
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))


class FetchError(RuntimeError):
    """kite-lite exited non-zero, timed out, or didn't print valid JSON."""


def fetch_page(url, timeout=30):
    """Run `kite-lite fetch <url>` and return the parsed page JSON."""
    try:
        proc = subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise FetchError(f"timeout fetching {url}")
    if proc.returncode != 0:
        raise FetchError(proc.stderr.strip() or f"exit code {proc.returncode}")
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as error:
        raise FetchError(f"salida de kite-lite no es JSON valido: {proc.stdout[:300]!r}") from error


def render_png(url, timeout=30):
    """Render `url` to PNG via kite-lite's `--png` shortcut and return the
    bytes. Goes through a throwaway temp file since kite-lite only writes
    a PNG to a path, not to stdout."""
    fd, path = tempfile.mkstemp(suffix=".png")
    os.close(fd)
    os.chmod(path, 0o666)
    try:
        proc = subprocess.run([BINARY, url, "--png", path], capture_output=True, text=True, timeout=timeout)
        if proc.returncode != 0:
            raise FetchError(proc.stderr.strip() or f"exit code {proc.returncode}")
        return Path(path).read_bytes()
    except subprocess.TimeoutExpired:
        raise FetchError(f"timeout rendering {url}")
    finally:
        os.unlink(path)


def parse_json_response(content):
    """Strip an optional ```json ... ``` (or bare ``` ... ```) fence some
    Ollama models still wrap JSON in even with format="json", then parse."""
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
