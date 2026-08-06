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

pub use js::{JsRuntime, JsValueResult};
pub use render::render_svg;

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
    use super::parse_html;

    #[test]
    fn extracts_agent_useful_page_data() {
        let page = parse_html(
            r#"<html><head><title>Example</title></head><body><h1>Hello</h1><a href='/docs'>Docs</a></body></html>"#,
        );
        assert_eq!(page.title.as_deref(), Some("Example"));
        assert!(page.text.contains("Hello"));
        assert_eq!(page.links, vec!["/docs"]);
    }
}
