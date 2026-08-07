"""Price monitor: watches a product's price across multiple sites and
alerts when it drops to or below a threshold. Persists the last known
price per site so it reports real changes across runs, not just the
same number every time.

Honest constraint: kite-lite's selector matching is tag-name only (see
find_selector in src/main.rs — no class/id/attribute support), so there's
no precise per-site "grab this exact element" targeting. This extracts
the price with a regex against the page's flattened text instead —
works without per-site configuration for simple product pages, with an
optional custom `pattern` per site for pages where the default doesn't
disambiguate (e.g. a page showing both a list price and a sale price).

Usage:
    python scripts/price_monitor.py <config.json> <state.json>

config.json:
{
  "products": [
    {"name": "...", "threshold": 50.0, "sites": [
      {"label": "...", "url": "...", "pattern": "optional custom regex, one capture group"}
    ]}
  ]
}

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import json
import os
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))
DEFAULT_PATTERN = r"\$\s?(\d{1,3}(?:[.,]\d{3})*(?:[.,]\d{2})?)"


def fetch_text(url):
    proc = subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"exit code {proc.returncode}")
    return json.loads(proc.stdout).get("text", "")


def extract_price(text, pattern):
    match = re.search(pattern, text)
    if not match:
        return None
    raw = match.group(1)
    cleaned = raw.replace(",", "") if raw.count(".") <= 1 else raw.replace(".", "").replace(",", ".")
    try:
        return float(cleaned)
    except ValueError:
        return None


def main():
    if len(sys.argv) != 3:
        print("usage: price_monitor.py <config.json> <state.json>", file=sys.stderr)
        sys.exit(2)
    config = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    state_path = Path(sys.argv[2])
    state = json.loads(state_path.read_text(encoding="utf-8")) if state_path.exists() else {}

    any_alert = False
    for product in config["products"]:
        name = product["name"]
        threshold = product.get("threshold")
        header = f"== {name} ==" + (f" (umbral: ${threshold:.2f})" if threshold is not None else "")
        print(f"\n{header}")
        for site in product["sites"]:
            key = f"{name}|{site['label']}"
            pattern = site.get("pattern", DEFAULT_PATTERN)
            try:
                text = fetch_text(site["url"])
                price = extract_price(text, pattern)
            except Exception as error:
                print(f"  [ERROR] {site['label']}: {error}")
                continue
            if price is None:
                print(f"  [SIN PRECIO] {site['label']}: no se encontro un precio con el patron")
                continue
            previous = state.get(key)
            trend = f" (antes: ${previous:.2f})" if previous is not None and previous != price else ""
            is_alert = threshold is not None and price <= threshold
            alert = " *** BAJO EL UMBRAL ***" if is_alert else ""
            any_alert = any_alert or is_alert
            print(f"  {site['label']}: ${price:.2f}{trend}{alert}")
            state[key] = price

    state_path.write_text(json.dumps(state, indent=2))
    if any_alert:
        sys.exit(1)


if __name__ == "__main__":
    main()
