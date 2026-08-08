"""Form protection auditor: looks for VISIBLE signals that a form has
some anti-spam defense — a suspiciously-named honeypot field (e.g.
"website"/"fax"/"honeypot") or a textual mention of "captcha" /
"recaptcha" / "hcaptcha" / "turnstile" — and flags forms with neither.

Hard limitation, stated upfront: kite-lite doesn't execute page
JavaScript and doesn't capture class/id/data-* attributes, so it
CANNOT see a real reCAPTCHA/hCaptcha/Turnstile widget — those are
injected by JS and identified by class/data attributes this project
doesn't track (see src/main.rs's find_selector: matching is tag-name
only). A "sin proteccion visible" result means exactly that — no
visible signal was found — NOT a confirmed absence of protection.
Treat this as a quick heuristic pass, not a security audit.

Usage:
    python scripts/form_protection_auditor.py <url|page.json>

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

HONEYPOT_NAMES = {
    "website", "url", "homepage", "fax", "honeypot", "hp_field",
    "hp_name", "bot_field", "spam_check", "address2", "company_website",
}
CAPTCHA_RE = re.compile(r"captcha|hcaptcha|recaptcha|turnstile", re.IGNORECASE)


def load_page(target):
    if target.startswith("http://") or target.startswith("https://"):
        proc = subprocess.run([BINARY, "fetch", target], capture_output=True, text=True, timeout=30)
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr.strip() or f"exit code {proc.returncode}")
        return json.loads(proc.stdout)
    return json.loads(Path(target).read_text(encoding="utf-8"))


def find_forms(element, out):
    if element.get("tag") == "form":
        out.append(element)
        return
    for child in element.get("children", []):
        find_forms(child, out)


def collect_field_names(element, names):
    if element.get("tag") in ("input", "textarea", "select") and element.get("name"):
        names.append(element["name"])
    for child in element.get("children", []):
        collect_field_names(child, names)


def audit_form(form):
    names = []
    collect_field_names(form, names)
    honeypot_hits = [n for n in names if n.lower() in HONEYPOT_NAMES]
    captcha_mentioned = bool(CAPTCHA_RE.search(form.get("text", "")))
    return {"fields": names, "honeypot_fields": honeypot_hits, "captcha_mentioned": captcha_mentioned}


def main():
    if len(sys.argv) != 2:
        print("usage: form_protection_auditor.py <url|page.json>", file=sys.stderr)
        sys.exit(2)

    page = load_page(sys.argv[1])
    forms = []
    find_forms(page["root"], forms)

    if not forms:
        print("no se encontraron <form> en la pagina")
        return

    unprotected = 0
    for i, form in enumerate(forms):
        result = audit_form(form)
        signals = []
        if result["honeypot_fields"]:
            signals.append(f"posible honeypot: campo(s) {result['honeypot_fields']}")
        if result["captcha_mentioned"]:
            signals.append("menciona captcha/recaptcha/hcaptcha/turnstile en su texto")

        print(f"[form {i}] campos: {result['fields']}")
        if signals:
            print(f"  proteccion detectada: {'; '.join(signals)}")
        else:
            print("  [WARN] sin senal visible de proteccion anti-spam (honeypot o mencion de captcha)")
            unprotected += 1

    if unprotected:
        print(f"\n{unprotected}/{len(forms)} formulario(s) sin señal visible de proteccion")
        sys.exit(1)


if __name__ == "__main__":
    main()
