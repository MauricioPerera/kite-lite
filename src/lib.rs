mod js;
mod render;

use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use serde::{Deserialize, Serialize};
use std::default::Default;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Element {
    pub tag: String,
    pub text: String,
    pub href: Option<String>,
    pub children: Vec<Element>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Page {
    #[serde(default)]
    pub url: Option<String>,
    pub title: Option<String>,
    pub text: String,
    pub links: Vec<String>,
    pub root: Element,
    #[serde(skip)]
    pub source: Option<String>,
}

/// The JSON contract between a fetcher/CLI process and the isolated
/// `kite-lite-js` evaluator process spoken over stdin/stdout.
#[derive(Debug, Deserialize, Serialize)]
pub struct EvalRequest {
    pub page: Page,
    pub script: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EvalResponse {
    pub value: Option<String>,
    pub error: Option<String>,
}

pub use js::{JsRuntime, JsValueResult};
pub use render::render_svg;

/// Rewrites every relative link (`page.links` and each `Element.href`) into
/// an absolute URL resolved against `base`. Hrefs that fail to parse (e.g.
/// `mailto:`, `javascript:`, or a malformed `base`) are left untouched.
pub fn resolve_links(page: &mut Page, base: &str) {
    let Ok(base_url) = url::Url::parse(base) else {
        return;
    };
    for link in &mut page.links {
        if let Ok(resolved) = base_url.join(link) {
            *link = resolved.to_string();
        }
    }
    resolve_element_hrefs(&mut page.root, &base_url);
}

fn resolve_element_hrefs(element: &mut Element, base: &url::Url) {
    if let Some(href) = &element.href {
        if let Ok(resolved) = base.join(href) {
            element.href = Some(resolved.to_string());
        }
    }
    for child in &mut element.children {
        resolve_element_hrefs(child, base);
    }
}

pub fn parse_html(source: &str) -> Page {
    let dom = parse_document(RcDom::default(), Default::default()).one(source.to_owned());
    let root = element_from_handle(&dom.document);
    let title = find_title(&dom.document);
    let mut links = Vec::new();
    collect_links(&dom.document, &mut links);
    let text = normalize_text(&root.text);

    Page {
        url: None,
        title,
        text,
        links,
        root,
        source: Some(source.to_string()),
    }
}

fn element_from_handle(handle: &Handle) -> Element {
    let (tag, href) = match &handle.data {
        NodeData::Element { name, attrs, .. } => {
            let href = attrs.borrow().iter().find_map(|attr| {
                (attr.name.local.as_ref() == "href").then(|| attr.value.to_string())
            });
            (name.local.to_string(), href)
        }
        NodeData::Document => ("document".to_string(), None),
        _ => ("text".to_string(), None),
    };

    if matches!(tag.as_str(), "head" | "script" | "style") {
        return Element {
            tag,
            text: String::new(),
            href,
            children: Vec::new(),
        };
    }

    let mut text = String::new();
    let mut children = Vec::new();
    for child in handle.children.borrow().iter() {
        match child.data {
            NodeData::Text { ref contents } => text.push_str(&contents.borrow()),
            NodeData::Element { .. } | NodeData::Document => {
                let child_element = element_from_handle(child);
                text.push_str(&child_element.text);
                children.push(child_element);
            }
            _ => {}
        }
    }

    Element {
        tag,
        text: normalize_text(&text),
        href,
        children,
    }
}

fn find_title(handle: &Handle) -> Option<String> {
    if let NodeData::Element { name, .. } = &handle.data {
        if name.local.as_ref() == "title" {
            let mut value = String::new();
            collect_text(handle, &mut value);
            let value = normalize_text(&value);
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    handle.children.borrow().iter().find_map(find_title)
}

fn collect_links(handle: &Handle, links: &mut Vec<String>) {
    if let NodeData::Element { name, attrs, .. } = &handle.data {
        if name.local.as_ref() == "a" {
            if let Some(href) = attrs.borrow().iter().find_map(|attr| {
                (attr.name.local.as_ref() == "href").then(|| attr.value.to_string())
            }) {
                links.push(href);
            }
        }
    }
    for child in handle.children.borrow().iter() {
        collect_links(child, links);
    }
}

fn collect_text(handle: &Handle, output: &mut String) {
    match &handle.data {
        NodeData::Text { contents } => output.push_str(&contents.borrow()),
        _ => {
            for child in handle.children.borrow().iter() {
                collect_text(child, output);
            }
        }
    }
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{parse_html, resolve_links, Element};

    #[test]
    fn extracts_agent_useful_page_data() {
        let page = parse_html(
            r#"<html><head><title>Example</title></head><body><h1>Hello</h1><a href='/docs'>Docs</a></body></html>"#,
        );
        assert_eq!(page.title.as_deref(), Some("Example"));
        assert!(page.text.contains("Hello"));
        assert_eq!(page.links, vec!["/docs"]);
    }

    fn collect_hrefs(element: &Element, hrefs: &mut Vec<String>) {
        if let Some(href) = &element.href {
            hrefs.push(href.clone());
        }
        for child in &element.children {
            collect_hrefs(child, hrefs);
        }
    }

    #[test]
    fn resolves_relative_links_against_base_url() {
        let mut page = parse_html(
            r#"<a href="/docs">Docs</a><a href="page2.html">Page2</a><a href="https://other.example/x">Abs</a>"#,
        );
        resolve_links(&mut page, "https://example.com/dir/index.html");

        assert_eq!(
            page.links,
            vec![
                "https://example.com/docs",
                "https://example.com/dir/page2.html",
                "https://other.example/x",
            ]
        );

        let mut hrefs = Vec::new();
        collect_hrefs(&page.root, &mut hrefs);
        assert_eq!(
            hrefs,
            vec![
                "https://example.com/docs",
                "https://example.com/dir/page2.html",
                "https://other.example/x",
            ]
        );
    }

    #[test]
    fn resolve_links_ignores_unparseable_base() {
        let mut page = parse_html(r#"<a href="/docs">Docs</a>"#);
        resolve_links(&mut page, "not a url");
        assert_eq!(page.links, vec!["/docs"]);
    }
}
