//! A minimal, practical on-page SEO linter — fourth sibling to the
//! webmcp/a11y/social linters. Deliberately doesn't repeat a11y-lint's
//! heading checks (multiple `<h1>`, level skips) even though they matter
//! for SEO too; this only covers what a11y-lint doesn't.

use crate::Page;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeoFinding {
    pub severity: Severity,
    pub message: String,
}

const MIN_TITLE_CHARS: usize = 10;
const MAX_TITLE_CHARS: usize = 60;
const MIN_DESCRIPTION_CHARS: usize = 50;
const MAX_DESCRIPTION_CHARS: usize = 160;
const MIN_WORD_COUNT: usize = 200;

fn meta_lookup<'a>(meta: &'a [(String, String)], key: &str) -> Option<&'a str> {
    meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn has_h1(element: &crate::Element) -> bool {
    element.tag == "h1" || element.children.iter().any(has_h1)
}

pub fn lint(page: &Page) -> Vec<SeoFinding> {
    let mut findings = Vec::new();
    let title = page.title.as_deref().unwrap_or("").trim();
    let description = meta_lookup(&page.meta, "description").unwrap_or("").trim();
    let robots = meta_lookup(&page.meta, "robots").unwrap_or("").to_ascii_lowercase();

    if title.is_empty() {
        findings.push(SeoFinding {
            severity: Severity::Error,
            message: "falta <title>: los buscadores usan esto como el enlace principal en los resultados".to_string(),
        });
    } else if title.chars().count() < MIN_TITLE_CHARS || title.chars().count() > MAX_TITLE_CHARS {
        findings.push(SeoFinding {
            severity: Severity::Warning,
            message: format!(
                "<title> tiene {} caracteres (recomendado {}-{}): {}",
                title.chars().count(),
                MIN_TITLE_CHARS,
                MAX_TITLE_CHARS,
                if title.chars().count() < MIN_TITLE_CHARS { "es muy corto/vago" } else { "se va a truncar en los resultados de busqueda" }
            ),
        });
    }

    if robots.contains("noindex") {
        findings.push(SeoFinding {
            severity: Severity::Error,
            message: format!("meta robots=\"{robots}\": esta pagina esta explicitamente excluida de la indexacion"),
        });
    }

    if description.is_empty() {
        findings.push(SeoFinding {
            severity: Severity::Warning,
            message: "falta <meta name=\"description\">: el buscador va a generar un fragmento automatico, sin control sobre el texto".to_string(),
        });
    } else if description.chars().count() < MIN_DESCRIPTION_CHARS || description.chars().count() > MAX_DESCRIPTION_CHARS {
        findings.push(SeoFinding {
            severity: Severity::Warning,
            message: format!(
                "meta description tiene {} caracteres (recomendado {}-{}): {}",
                description.chars().count(),
                MIN_DESCRIPTION_CHARS,
                MAX_DESCRIPTION_CHARS,
                if description.chars().count() < MIN_DESCRIPTION_CHARS { "es muy corta" } else { "se va a truncar en los resultados de busqueda" }
            ),
        });
    }

    if !has_h1(&page.root) {
        findings.push(SeoFinding {
            severity: Severity::Warning,
            message: "no hay ningun <h1> en la pagina: es la senal mas clara del tema principal para el buscador".to_string(),
        });
    }

    let word_count = page.text.split_whitespace().count();
    if word_count < MIN_WORD_COUNT {
        findings.push(SeoFinding {
            severity: Severity::Info,
            message: format!("la pagina tiene {word_count} palabras (recomendado {MIN_WORD_COUNT}+): contenido muy corto puede rankear peor"),
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_html;

    fn messages(findings: &[SeoFinding]) -> Vec<&str> {
        findings.iter().map(|f| f.message.as_str()).collect()
    }

    fn long_text(words: usize) -> String {
        "palabra ".repeat(words)
    }

    #[test]
    fn a_well_formed_page_produces_no_findings() {
        let html = format!(
            r#"<html><head><title>Un titulo de longitud razonable</title>
                 <meta name="description" content="Una descripcion de longitud razonable que describe bien el contenido de la pagina para los buscadores.">
               </head><body><h1>Titulo</h1><p>{}</p></body></html>"#,
            long_text(250)
        );
        let page = parse_html(&html);
        assert!(lint(&page).is_empty(), "{:?}", lint(&page));
    }

    #[test]
    fn missing_title_is_an_error() {
        let page = parse_html("<body><h1>Hola</h1></body>");
        assert!(messages(&lint(&page)).iter().any(|m| m.contains("falta <title>")));
    }

    #[test]
    fn noindex_is_an_error() {
        let page = parse_html(r#"<html><head><title>Titulo largo y razonable aca</title><meta name="robots" content="noindex, nofollow"></head></html>"#);
        let findings = lint(&page);
        assert!(findings.iter().any(|f| f.severity == Severity::Error && f.message.contains("noindex")));
    }

    #[test]
    fn short_title_is_a_warning() {
        let page = parse_html("<title>Hola</title>");
        assert!(messages(&lint(&page)).iter().any(|m| m.contains("muy corto")));
    }

    #[test]
    fn missing_description_is_a_warning() {
        let page = parse_html("<title>Un titulo de longitud razonable aca</title>");
        assert!(messages(&lint(&page)).iter().any(|m| m.contains("falta <meta name=\"description\">")));
    }

    #[test]
    fn missing_h1_is_a_warning() {
        let page = parse_html(
            r#"<html><head><title>Un titulo de longitud razonable</title>
                 <meta name="description" content="Una descripcion de longitud razonable que describe bien el contenido de la pagina."></head>
               <body><h2>Subtitulo</h2></body></html>"#,
        );
        assert!(messages(&lint(&page)).iter().any(|m| m.contains("no hay ningun <h1>")));
    }

    #[test]
    fn thin_content_is_an_info_note() {
        let page = parse_html(
            r#"<html><head><title>Un titulo de longitud razonable</title>
                 <meta name="description" content="Una descripcion de longitud razonable que describe bien el contenido de la pagina."></head>
               <body><h1>Titulo</h1><p>Poco texto aca.</p></body></html>"#,
        );
        assert!(messages(&lint(&page)).iter().any(|m| m.contains("palabras")));
    }
}
