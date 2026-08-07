"""Contact info extractor: walks one or more pages looking for emails,
phone numbers, and social media profile links, and builds a structured
contact card. Prioritizes high-confidence signals (mailto:/tel: links)
over a text regex fallback for emails/phones not wrapped in a link.

Honest limitation: phone matching is a heuristic regex, not a real
phone-format parser (like libphonenumber) — expect some false
positives/negatives across international formats. It's a first pass,
not a validated result.

Usage:
    python scripts/contact_extractor.py <url1> [url2 ...] [--json]

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import json
import os
import re
import subprocess
import sys
import urllib.parse
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / (
    "kite-lite.exe" if os.name == "nt" else "kite-lite"
)
BINARY = os.environ.get("KITE_LITE_BIN", str(DEFAULT_BINARY))

EMAIL_RE = re.compile(r"[\w.+-]+@[\w-]+\.[\w.-]+")
PHONE_RE = re.compile(r"(?:\+\d{1,3}[\s.-]?)?\(?\d{2,4}\)?[\s.-]?\d{3,4}[\s.-]?\d{3,4}(?:[\s.-]?\d{2,4})?")
SOCIAL_DOMAINS = {
    "twitter.com": "twitter",
    "x.com": "twitter",
    "facebook.com": "facebook",
    "instagram.com": "instagram",
    "linkedin.com": "linkedin",
    "github.com": "github",
    "youtube.com": "youtube",
    "tiktok.com": "tiktok",
}


def fetch_page(url):
    proc = subprocess.run([BINARY, "fetch", url], capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"exit code {proc.returncode}")
    return json.loads(proc.stdout)


def walk_links(element, emails, phones):
    href = element.get("href")
    if href:
        if href.startswith("mailto:"):
            emails.add(href[len("mailto:") :].split("?")[0])
        elif href.startswith("tel:"):
            phones.add(href[len("tel:") :])
    for child in element.get("children", []):
        walk_links(child, emails, phones)


def extract_social(links, social):
    for link in links:
        domain = urllib.parse.urlparse(link).netloc.lower()
        domain = domain[4:] if domain.startswith("www.") else domain
        name = SOCIAL_DOMAINS.get(domain)
        if name:
            social.setdefault(name, set()).add(link)


def extract_from_text(text, emails, phones):
    for match in EMAIL_RE.findall(text):
        emails.add(match)
    for match in PHONE_RE.findall(text):
        digit_count = sum(c.isdigit() for c in match)
        if digit_count >= 7:  # avoid flagging dates/short numbers as phones
            phones.add(match.strip())


def main():
    args = sys.argv[1:]
    urls = [a for a in args if a != "--json"]
    as_json = "--json" in args
    if not urls:
        print("usage: contact_extractor.py <url1> [url2 ...] [--json]", file=sys.stderr)
        sys.exit(2)

    emails, phones, social = set(), set(), {}
    for url in urls:
        print(f"fetching {url}", file=sys.stderr)
        page = fetch_page(url)
        walk_links(page["root"], emails, phones)
        extract_social(page.get("links", []), social)
        extract_from_text(page.get("text", ""), emails, phones)

    result = {
        "emails": sorted(emails),
        "phones": sorted(phones),
        "social": {k: sorted(v) for k, v in social.items()},
        "sources": urls,
    }

    if as_json:
        print(json.dumps(result, indent=2, ensure_ascii=False))
    else:
        print(f"\nEmails: {', '.join(result['emails']) or '(ninguno)'}")
        print(f"Telefonos: {', '.join(result['phones']) or '(ninguno)'}")
        for name, links in result["social"].items():
            print(f"{name.capitalize()}: {', '.join(links)}")


if __name__ == "__main__":
    main()
