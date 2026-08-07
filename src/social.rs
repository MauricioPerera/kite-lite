//! Simulates what a link-unfurling bot (Twitter/X, Slack, Facebook, iMessage,
//! WhatsApp...) would show when someone shares a page: resolves the title,
//! description, and image through the same fallback chain those crawlers
//! use (Open Graph, then Twitter Card, then plain `<meta name="description">`
//! /`<title>`), and flags the common ways that preview ends up degraded.

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
pub struct SocialFinding {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SocialPreview {
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
}

fn meta_lookup<'a>(meta: &'a [(String, String)], key: &str) -> Option<&'a str> {
    meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn first_words(text: &str, max_chars: usize) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    let truncated: String = text.chars().take(max_chars).collect();
    Some(truncated)
}

/// Resolves the preview using the same priority order real crawlers do:
/// Open Graph, then Twitter Card, then plain meta/heuristics.
pub fn resolve_preview(page: &Page) -> SocialPreview {
    let title = meta_lookup(&page.meta, "og:title")
        .or_else(|| meta_lookup(&page.meta, "twitter:title"))
        .map(str::to_string)
        .or_else(|| page.title.clone());

    let description = meta_lookup(&page.meta, "og:description")
        .or_else(|| meta_lookup(&page.meta, "twitter:description"))
        .or_else(|| meta_lookup(&page.meta, "description"))
        .map(str::to_string)
        .or_else(|| first_words(&page.text, 200));

    let image = meta_lookup(&page.meta, "og:image")
        .or_else(|| meta_lookup(&page.meta, "twitter:image"))
        .map(str::to_string);

    SocialPreview { title, description, image }
}

pub fn lint(page: &Page) -> (SocialPreview, Vec<SocialFinding>) {
    let preview = resolve_preview(page);
    let mut findings = Vec::new();

    if preview.title.is_none() {
        findings.push(SocialFinding {
            severity: Severity::Error,
            message: "sin titulo resoluble (ni og:title, ni twitter:title, ni <title>): la mayoria de los bots mostrara solo la URL desnuda".to_string(),
        });
    }

    if preview.image.is_none() {
        findings.push(SocialFinding {
            severity: Severity::Warning,
            message: "sin imagen (ni og:image ni twitter:image): el preview se vera degradado en la mayoria de las plataformas".to_string(),
        });
    }

    if preview.description.is_none() {
        findings.push(SocialFinding {
            severity: Severity::Warning,
            message: "sin descripcion resoluble (ni meta description/OG/Twitter, ni texto en la pagina)".to_string(),
        });
    } else if let Some(description) = &preview.description {
        if description.chars().count() > 200 {
            findings.push(SocialFinding {
                severity: Severity::Info,
                message: format!(
                    "la descripcion tiene {} caracteres: la mayoria de las plataformas la recorta alrededor de 200",
                    description.chars().count()
                ),
            });
        }
    }

    if let Some(image) = &preview.image {
        if !image.starts_with("http://") && !image.starts_with("https://") {
            findings.push(SocialFinding {
                severity: Severity::Info,
                message: format!("la imagen resuelta ('{image}') no es una URL absoluta: muchos crawlers no la resuelven contra la pagina y la ignoran"),
            });
        }
    }

    (preview, findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_html;

    fn messages(findings: &[SocialFinding]) -> Vec<&str> {
        findings.iter().map(|f| f.message.as_str()).collect()
    }

    #[test]
    fn prefers_open_graph_over_title_and_text() {
        let page = parse_html(
            r#"<html><head><title>Fallback title</title>
                 <meta property="og:title" content="OG Title">
                 <meta property="og:description" content="OG description">
                 <meta property="og:image" content="https://example.com/img.png">
               </head><body>Some body text</body></html>"#,
        );
        let (preview, findings) = lint(&page);
        assert_eq!(preview.title.as_deref(), Some("OG Title"));
        assert_eq!(preview.description.as_deref(), Some("OG description"));
        assert_eq!(preview.image.as_deref(), Some("https://example.com/img.png"));
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn falls_back_to_twitter_card_then_plain_meta() {
        let page = parse_html(
            r#"<html><head>
                 <meta name="twitter:title" content="Twitter Title">
                 <meta name="description" content="Plain description">
               </head><body></body></html>"#,
        );
        let (preview, _) = lint(&page);
        assert_eq!(preview.title.as_deref(), Some("Twitter Title"));
        assert_eq!(preview.description.as_deref(), Some("Plain description"));
    }

    #[test]
    fn falls_back_to_html_title_and_body_text() {
        let page = parse_html(r#"<title>Just a title</title><body>Just some body text here.</body>"#);
        let (preview, findings) = lint(&page);
        assert_eq!(preview.title.as_deref(), Some("Just a title"));
        assert_eq!(preview.description.as_deref(), Some("Just some body text here."));
        assert!(messages(&findings).iter().any(|m| m.contains("sin imagen")));
    }

    #[test]
    fn missing_everything_is_an_error_for_title() {
        let page = parse_html("<body></body>");
        let (preview, findings) = lint(&page);
        assert!(preview.title.is_none());
        assert!(findings.iter().any(|f| f.severity == Severity::Error && f.message.contains("sin titulo")));
    }

    #[test]
    fn long_description_is_an_info_note() {
        let long = "a".repeat(250);
        let page = parse_html(&format!(r#"<meta name="description" content="{long}">"#));
        let (_, findings) = lint(&page);
        assert!(messages(&findings).iter().any(|m| m.contains("200")));
    }

    #[test]
    fn relative_image_url_is_an_info_note() {
        let page = parse_html(r#"<meta property="og:image" content="/img.png">"#);
        let (_, findings) = lint(&page);
        assert!(messages(&findings).iter().any(|m| m.contains("no es una URL absoluta")));
    }
}
