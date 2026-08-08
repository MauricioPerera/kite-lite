"""Cookie/privacy auditor: navigates to a URL and reports exactly which
cookies the server set on that very first load — before any user
interaction — using kite-lite's Set-Cookie capture (added to the core
for this: see Page.cookies / parse_set_cookie in src/lib.rs). Flags
cookies with common tracking-related names, and whether the page shows
any visible mention of cookies/consent at all.

Honest limitation: this only sees Set-Cookie response headers on GET
requests kite-lite itself makes — it cannot see cookies set later by
page JavaScript (kite-lite doesn't execute it), so a "no cookies on
first load" result describes only the initial HTTP response, not
everything a real browser session might eventually pick up.

Usage:
    python scripts/cookie_privacy_auditor.py <url>

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

TRACKING_NAME_RE = re.compile(
    r"^(_ga|_gid|_gcl|_fbp|_fbc|_gat|_uetsid|_uetvid|_pin_unauth)|track|analytics|ads?_id",
    re.IGNORECASE,
)
CONSENT_TEXT_RE = re.compile(r"cookie|consent|consentimiento|privacidad|privacy", re.IGNORECASE)


def fetch_page(url):
    proc = subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"exit code {proc.returncode}")
    return json.loads(proc.stdout)


def main():
    if len(sys.argv) != 2:
        print("usage: cookie_privacy_auditor.py <url>", file=sys.stderr)
        sys.exit(2)
    url = sys.argv[1]

    page = fetch_page(url)
    cookies = page.get("cookies", [])
    has_consent_text = bool(CONSENT_TEXT_RE.search(page.get("text", "")))

    print(f"URL: {page.get('url') or url}")
    print(f"Titulo: {page.get('title')}")
    print(f"Cookies en la primera carga (antes de cualquier interaccion): {len(cookies)}")

    if not cookies:
        print("No se establecieron cookies en la carga inicial.")
        return

    tracking_count = 0
    for cookie in cookies:
        is_tracking = bool(TRACKING_NAME_RE.search(cookie["name"]))
        tracking_count += is_tracking
        flags = []
        if cookie.get("secure"):
            flags.append("Secure")
        if cookie.get("http_only"):
            flags.append("HttpOnly")
        if cookie.get("same_site"):
            flags.append(f"SameSite={cookie['same_site']}")
        label = " [posible tracking]" if is_tracking else ""
        print(f"  - {cookie['name']}={cookie['value']!r} {' '.join(flags)}{label}")

    print(f"\nMenciona cookies/consentimiento en el texto de la pagina: {'si' if has_consent_text else 'NO'}")
    if tracking_count and not has_consent_text:
        print(
            f"[ADVERTENCIA] {tracking_count} cookie(s) con nombre de tracking se establecieron "
            "antes de cualquier interaccion, y la pagina no menciona cookies/consentimiento visiblemente."
        )
        sys.exit(1)


if __name__ == "__main__":
    main()
