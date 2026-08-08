"""Phishing/clone detector: fetches a known-legitimate URL and a suspect
URL via kite-lite's full page snapshot (title, text, and every form's
resolved action + field names) and flags a suspect page that looks nearly
identical in content but either lives on a different domain, or — the
classic credential-phishing move — routes a form to a THIRD domain that
belongs to neither page (clone the page, redirect where the data goes).

Usage:
    python scripts/phishing_detector.py <legit_url> <suspect_url>

Purely structural/heuristic and content-based — not a replacement for a
real threat-intel feed, but a fast, explainable, dependency-free first
pass. Exits non-zero when risk is MEDIO or ALTO.

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import difflib
import sys
import urllib.parse

from _kite_lite import fetch_page


def collect_forms(element, forms):
    if element.get("tag") == "form":
        fields = []
        collect_field_names(element, fields)
        forms.append({"action": element.get("href"), "fields": fields})
    for child in element.get("children", []):
        collect_forms(child, forms)


def collect_field_names(element, fields):
    if element.get("tag") in ("input", "textarea", "select") and element.get("name"):
        fields.append(element["name"])
    for child in element.get("children", []):
        collect_field_names(child, fields)


def domain_of(url):
    return urllib.parse.urlparse(url or "").netloc.lower()


def main():
    if len(sys.argv) != 3:
        print("usage: phishing_detector.py <legit_url> <suspect_url>", file=sys.stderr)
        sys.exit(2)
    legit_url, suspect_url = sys.argv[1], sys.argv[2]

    legit = fetch_page(legit_url)
    suspect = fetch_page(suspect_url)

    legit_domain = domain_of(legit.get("url") or legit_url)
    suspect_domain = domain_of(suspect.get("url") or suspect_url)
    same_domain = legit_domain == suspect_domain

    title_similarity = difflib.SequenceMatcher(None, legit.get("title") or "", suspect.get("title") or "").ratio()
    text_similarity = difflib.SequenceMatcher(None, legit.get("text") or "", suspect.get("text") or "").ratio()

    legit_forms = []
    collect_forms(legit["root"], legit_forms)
    suspect_forms = []
    collect_forms(suspect["root"], suspect_forms)
    legit_field_sets = [set(f["fields"]) for f in legit_forms if f["fields"]]

    suspicious_forms = []
    for form in suspect_forms:
        action_domain = domain_of(form["action"]) if form["action"] else None
        exfil_to_third_party = bool(action_domain) and action_domain != suspect_domain and action_domain != legit_domain
        field_overlap = any(set(form["fields"]) & legit_fields for legit_fields in legit_field_sets)
        if exfil_to_third_party or (field_overlap and action_domain and action_domain != suspect_domain):
            suspicious_forms.append(
                {**form, "action_domain": action_domain, "exfil_to_third_party": exfil_to_third_party, "field_overlap_with_legit": field_overlap}
            )

    content_looks_cloned = text_similarity > 0.8 or title_similarity > 0.8

    risk = "BAJO"
    reasons = []
    if content_looks_cloned and not same_domain:
        reasons.append(f"contenido {text_similarity:.0%} similar pero dominio distinto ({suspect_domain} vs {legit_domain})")
        risk = "MEDIO"
    if suspicious_forms:
        reasons.append(f"{len(suspicious_forms)} formulario(s) envian datos fuera del dominio de la pagina que los sirve")
        risk = "ALTO"

    print(f"Legit:    {legit_url} (dominio: {legit_domain})")
    print(f"Sospecha: {suspect_url} (dominio: {suspect_domain})")
    print(f"Similitud de titulo: {title_similarity:.1%}  |  Similitud de texto: {text_similarity:.1%}")
    print(f"Mismo dominio: {same_domain}")
    for form in suspicious_forms:
        print(
            f"  [SOSPECHOSO] form -> {form['action']} (dominio: {form['action_domain']}) "
            f"campos={form['fields']} exfil_a_tercero={form['exfil_to_third_party']}"
        )
    print(f"\nRIESGO: {risk}")
    for reason in reasons:
        print(f"  - {reason}")

    sys.exit(0 if risk == "BAJO" else 1)


if __name__ == "__main__":
    main()
