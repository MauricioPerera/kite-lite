"""Cookie/privacy auditor: navigates to a URL and reports exactly which
cookies the server set on that very first load — before any user
interaction — using kite-lite's Set-Cookie capture (added to the core
for this: see Page.cookies / parse_set_cookie in kite-lite-core/src/lib.rs). Flags
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

import re
import sys

from _kite_lite import fetch_page

# Known tracker-cookie prefixes, plus generic keyword matches. The keyword
# matches alone would also catch opt-out/consent cookies that happen to
# contain "track"/"analytics" in their name (e.g. "analytics_consent",
# "ad_blocked") -- OPT_OUT_NAME_RE below excludes those.
TRACKING_NAME_RE = re.compile(
    r"^(_ga|_gid|_gcl|_fbp|_fbc|_gat|_uetsid|_uetvid|_pin_unauth)|track|analytics|ads?_id",
    re.IGNORECASE,
)
OPT_OUT_NAME_RE = re.compile(r"consent|opt.?out|block", re.IGNORECASE)

# A page merely containing the word "privacy"/"privacidad" anywhere (e.g. a
# footer link to a privacy policy) is not evidence of an actual cookie
# consent mechanism -- require the word "cookie(s)" AND a consent-related
# action/policy word to both appear before treating the page as having
# disclosed anything about cookies.
COOKIE_MENTION_RE = re.compile(r"\bcookies?\b", re.IGNORECASE)
CONSENT_SIGNAL_RE = re.compile(
    r"consent(imiento)?|acepta|rechaza|gestiona|preferencias|banner|policy|pol[ií]tica",
    re.IGNORECASE,
)


def main():
    if len(sys.argv) != 2:
        print("usage: cookie_privacy_auditor.py <url>", file=sys.stderr)
        sys.exit(2)
    url = sys.argv[1]

    page = fetch_page(url)
    cookies = page.get("cookies", [])
    text = page.get("text", "")
    has_consent_text = bool(COOKIE_MENTION_RE.search(text) and CONSENT_SIGNAL_RE.search(text))

    print(f"URL: {page.get('url') or url}")
    print(f"Titulo: {page.get('title')}")
    print(f"Cookies en la primera carga (antes de cualquier interaccion): {len(cookies)}")

    if not cookies:
        print("No se establecieron cookies en la carga inicial.")
        return

    tracking_count = 0
    for cookie in cookies:
        is_tracking = bool(TRACKING_NAME_RE.search(cookie["name"])) and not OPT_OUT_NAME_RE.search(cookie["name"])
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
