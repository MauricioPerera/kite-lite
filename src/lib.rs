mod js;
mod layout;
mod raster;
mod render;

use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use serde::{Deserialize, Serialize};
use std::default::Default;

/// An element's computed box after `compute_layout` runs: `x`/`y` are its
/// top-left corner and `width`/`height` its size, all in the same units as
/// the `viewport_width` passed to `compute_layout`. `x` is always `0.0` in
/// the current implementation — layout only stacks elements vertically,
/// there's no horizontal positioning (indentation, columns, floats) yet.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct Layout {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Element {
    pub tag: String,
    pub text: String,
    /// The element's primary URL-ish attribute: `href` for `<a>`, `action`
    /// for `<form>` (so it participates in `resolve_links`/navigation the
    /// same way a link does). `None` for every other tag.
    pub href: Option<String>,
    pub children: Vec<Element>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Layout>,
    /// The current value of an `<input>`/`<textarea>`: the `value`
    /// attribute (or the element's own text, for `<textarea>`) at parse
    /// time, mutable afterward via CDP's `Input.dispatchKeyEvent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The `name` attribute, used to build a form's submitted query string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The `type` attribute (named `kind` since `type` is a keyword),
    /// currently only inspected on `<input>` to tell a submit button
    /// (`type="submit"`) apart from a text field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
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
pub use layout::compute_layout;
pub use raster::{render_pdf, render_png};
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
    let (tag, href, value, name, kind) = match &handle.data {
        NodeData::Element { name: tag_name, attrs, .. } => {
            let attrs = attrs.borrow();
            let tag = tag_name.local.to_string();
            let href_attr_name = if tag == "form" { "action" } else { "href" };
            let attr = |attr_name: &str| -> Option<String> {
                attrs
                    .iter()
                    .find(|attr| attr.name.local.as_ref() == attr_name)
                    .map(|attr| attr.value.to_string())
            };
            (tag, attr(href_attr_name), attr("value"), attr("name"), attr("type"))
        }
        NodeData::Document => ("document".to_string(), None, None, None, None),
        _ => ("text".to_string(), None, None, None, None),
    };

    if matches!(tag.as_str(), "head" | "script" | "style") {
        return Element {
            tag,
            text: String::new(),
            href,
            children: Vec::new(),
            layout: None,
            value,
            name,
            kind,
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
    let text = normalize_text(&text);

    // <textarea>initial value</textarea> sets its value via child text, not
    // a `value` attribute.
    let value = if tag == "textarea" && value.is_none() {
        Some(text.clone())
    } else {
        value
    };

    Element {
        tag,
        text,
        href,
        children,
        layout: None,
        value,
        name,
        kind,
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

    fn find_by_tag<'a>(element: &'a Element, tag: &str) -> Option<&'a Element> {
        if element.tag == tag {
            return Some(element);
        }
        element.children.iter().find_map(|child| find_by_tag(child, tag))
    }

    #[test]
    fn captures_input_value_name_and_kind() {
        let page = parse_html(r#"<input type="text" name="q" value="hello">"#);
        let input = find_by_tag(&page.root, "input").expect("input missing");
        assert_eq!(input.value.as_deref(), Some("hello"));
        assert_eq!(input.name.as_deref(), Some("q"));
        assert_eq!(input.kind.as_deref(), Some("text"));
    }

    #[test]
    fn textarea_defaults_value_to_its_text_content() {
        let page = parse_html(r#"<textarea name="bio">Initial text</textarea>"#);
        let textarea = find_by_tag(&page.root, "textarea").expect("textarea missing");
        assert_eq!(textarea.value.as_deref(), Some("Initial text"));
    }

    #[test]
    fn form_action_is_captured_as_href_and_gets_resolved() {
        let mut page = parse_html(r#"<form action="/search"><input name="q"></form>"#);
        resolve_links(&mut page, "https://example.com/dir/index.html");
        let form = find_by_tag(&page.root, "form").expect("form missing");
        assert_eq!(form.href.as_deref(), Some("https://example.com/search"));
    }
}
