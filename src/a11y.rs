//! A minimal, practical accessibility linter over the already-parsed DOM:
//! images without `alt`, heading levels that skip (h1 straight to h3),
//! more than one `<h1>`, links with no accessible text, and a missing
//! `lang` on `<html>`. Not a WCAG conformance checker — a handful of the
//! most common, cheaply-detectable mistakes, in the same spirit as the
//! WebMCP linter.

use crate::{Element, Page};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct A11yFinding {
    pub severity: Severity,
    pub message: String,
}

pub fn lint(page: &Page) -> Vec<A11yFinding> {
    let mut findings = Vec::new();
    check_images(&page.root, &mut findings);
    check_links(&page.root, &mut findings);
    check_lang(&page.root, &mut findings);

    let mut headings = Vec::new();
    collect_headings(&page.root, &mut headings);
    check_heading_hierarchy(&headings, &mut findings);

    findings
}

fn check_images(element: &Element, findings: &mut Vec<A11yFinding>) {
    if element.tag == "img" && element.alt.is_none() {
        findings.push(A11yFinding {
            severity: Severity::Warning,
            message: "<img> sin atributo 'alt': un lector de pantalla no puede describirla (usa alt=\"\" si es decorativa)".to_string(),
        });
    }
    for child in &element.children {
        check_images(child, findings);
    }
}

fn check_links(element: &Element, findings: &mut Vec<A11yFinding>) {
    if element.tag == "a" {
        let has_text = !element.text.trim().is_empty();
        let has_described_image = has_descendant_with_alt(element);
        if !has_text && !has_described_image {
            findings.push(A11yFinding {
                severity: Severity::Warning,
                message: format!(
                    "<a href=\"{}\"> sin texto ni imagen con 'alt': un lector de pantalla no anuncia nada util",
                    element.href.as_deref().unwrap_or("")
                ),
            });
        }
    }
    for child in &element.children {
        check_links(child, findings);
    }
}

fn has_descendant_with_alt(element: &Element) -> bool {
    if element.tag == "img" && element.alt.as_deref().is_some_and(|alt| !alt.is_empty()) {
        return true;
    }
    element.children.iter().any(has_descendant_with_alt)
}

fn check_lang(root: &Element, findings: &mut Vec<A11yFinding>) {
    match find_html_element(root) {
        Some(html) if html.lang.as_deref().unwrap_or("").trim().is_empty() => {
            findings.push(A11yFinding {
                severity: Severity::Warning,
                message: "<html> sin atributo 'lang': un lector de pantalla no sabe en que idioma anunciar la pagina".to_string(),
            });
        }
        _ => {}
    }
}

fn find_html_element(element: &Element) -> Option<&Element> {
    if element.tag == "html" {
        return Some(element);
    }
    element.children.iter().find_map(find_html_element)
}

fn collect_headings<'a>(element: &'a Element, headings: &mut Vec<&'a Element>) {
    if matches!(element.tag.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
        headings.push(element);
    }
    for child in &element.children {
        collect_headings(child, headings);
    }
}

fn heading_level(tag: &str) -> u8 {
    tag.as_bytes()[1] - b'0'
}

fn check_heading_hierarchy(headings: &[&Element], findings: &mut Vec<A11yFinding>) {
    let h1_count = headings.iter().filter(|h| h.tag == "h1").count();
    if h1_count > 1 {
        findings.push(A11yFinding {
            severity: Severity::Warning,
            message: format!("hay {h1_count} elementos <h1> en la pagina: deberia haber un unico encabezado principal"),
        });
    }

    let mut previous_level: Option<u8> = None;
    for heading in headings {
        let level = heading_level(&heading.tag);
        if let Some(previous) = previous_level {
            if level > previous + 1 {
                findings.push(A11yFinding {
                    severity: Severity::Warning,
                    message: format!(
                        "salto de nivel de encabezado: <h{previous}> seguido de <h{level}> (deberia ser <h{}>) - texto: '{}'",
                        previous + 1,
                        heading.text.chars().take(40).collect::<String>()
                    ),
                });
            }
        }
        previous_level = Some(level);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_html;

    fn messages(findings: &[A11yFinding]) -> Vec<&str> {
        findings.iter().map(|f| f.message.as_str()).collect()
    }

    #[test]
    fn a_well_formed_page_produces_no_findings() {
        let page = parse_html(
            r#"<html lang="es"><body><h1>Titulo</h1><h2>Subtitulo</h2>
                 <img src="x.png" alt="una foto">
                 <a href="/x">Ir a x</a>
               </body></html>"#,
        );
        assert!(lint(&page).is_empty());
    }

    #[test]
    fn image_without_alt_is_flagged() {
        let page = parse_html(r#"<img src="x.png">"#);
        assert!(messages(&lint(&page)).iter().any(|m| m.contains("sin atributo 'alt'")));
    }

    #[test]
    fn image_with_empty_alt_is_not_flagged() {
        let page = parse_html(r#"<html lang="es"><img src="x.png" alt=""></html>"#);
        assert!(lint(&page).is_empty());
    }

    #[test]
    fn heading_level_skip_is_flagged() {
        let page = parse_html(r#"<h1>Titulo</h1><h3>Subtitulo</h3>"#);
        assert!(messages(&lint(&page)).iter().any(|m| m.contains("salto de nivel")));
    }

    #[test]
    fn multiple_h1_is_flagged() {
        let page = parse_html(r#"<h1>Uno</h1><h1>Dos</h1>"#);
        assert!(messages(&lint(&page)).iter().any(|m| m.contains("2 elementos <h1>")));
    }

    #[test]
    fn empty_link_without_image_is_flagged() {
        let page = parse_html(r#"<a href="/x"></a>"#);
        assert!(messages(&lint(&page)).iter().any(|m| m.contains("sin texto ni imagen")));
    }

    #[test]
    fn link_with_described_image_is_not_flagged() {
        let page = parse_html(r#"<html lang="es"><a href="/x"><img src="icon.png" alt="ir a x"></a></html>"#);
        assert!(lint(&page).is_empty());
    }

    #[test]
    fn missing_lang_is_flagged() {
        let page = parse_html(r#"<html><body><p>hola</p></body></html>"#);
        assert!(messages(&lint(&page)).iter().any(|m| m.contains("sin atributo 'lang'")));
    }
}
