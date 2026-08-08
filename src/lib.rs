mod a11y;
mod js;
mod layout;
mod raster;
mod render;
mod seo;
mod social;
mod webmcp;

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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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
    /// Declarative WebMCP tool name (the `toolname` attribute on a `<form>`;
    /// see <https://github.com/webmachinelearning/webmcp/blob/main/declarative-api-explainer.md>).
    /// A form only becomes a discoverable tool once this is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// `tooldescription` on a `<form>`: the tool's description in its
    /// generated MCP schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_description: Option<String>,
    /// Presence of the boolean `toolautosubmit` attribute on a `<form>`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tool_autosubmit: bool,
    /// `toolparamdescription` on a form field: that property's description
    /// in the generated input schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_param_description: Option<String>,
    /// Presence of the boolean `required` attribute on a form field, which
    /// feeds the generated input schema's `required` list.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// The `method` attribute on a `<form>` (e.g. `"post"`), used by the
    /// WebMCP linter to flag forms kite-lite can't actually submit — it
    /// only ever simulates a GET.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The `alt` attribute (any tag, only meaningful on `<img>`): `None`
    /// means the attribute is missing entirely (an accessibility problem),
    /// distinct from `Some(String::new())` (`alt=""`, valid for decorative
    /// images).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    /// The `lang` attribute (checked on the document's `<html>` element).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Page {
    #[serde(default)]
    pub url: Option<String>,
    pub title: Option<String>,
    pub text: String,
    pub links: Vec<String>,
    pub root: Element,
    /// Every `<meta name="..." content="...">` or `<meta property="..."
    /// content="...">` in `<head>`, keyed by `name` (falling back to
    /// `property`) in source order — e.g. `og:title`, `twitter:image`,
    /// `description`. `<head>`'s children are otherwise discarded from
    /// `root` (see `element_from_handle`), so this is the only place
    /// meta tags surface.
    #[serde(default)]
    pub meta: Vec<(String, String)>,
    /// Cookies set by the server on this fetch/navigation, parsed from
    /// each `Set-Cookie` response header — see `parse_set_cookie`. Empty
    /// for anything not backed by a real HTTP response (`parse_html`
    /// alone never populates this; only `fetch_page`/`navigate_page` in
    /// main.rs do, since they're the ones holding the response headers).
    #[serde(default)]
    pub cookies: Vec<CookieInfo>,
    #[serde(skip)]
    pub source: Option<String>,
}

/// A cookie parsed from one `Set-Cookie` response header — see
/// `parse_set_cookie`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CookieInfo {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secure: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub http_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

/// Parses one raw `Set-Cookie` header value (RFC 6265) into structured
/// fields. A hand-rolled parser (no cookie crate dependency) covering
/// the common case: `name=value; Attr=val; Flag`. Returns `None` for a
/// header with no `name=value` pair at all.
pub fn parse_set_cookie(raw: &str) -> Option<CookieInfo> {
    let mut parts = raw.split(';');
    let (name, value) = parts.next()?.trim().split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    let mut cookie = CookieInfo {
        name: name.to_string(),
        value: value.trim().to_string(),
        domain: None,
        path: None,
        secure: false,
        http_only: false,
        same_site: None,
    };
    for part in parts {
        let part = part.trim();
        if let Some((key, val)) = part.split_once('=') {
            match key.trim().to_ascii_lowercase().as_str() {
                "domain" => cookie.domain = Some(val.trim().to_string()),
                "path" => cookie.path = Some(val.trim().to_string()),
                "samesite" => cookie.same_site = Some(val.trim().to_string()),
                _ => {}
            }
        } else {
            match part.to_ascii_lowercase().as_str() {
                "secure" => cookie.secure = true,
                "httponly" => cookie.http_only = true,
                _ => {}
            }
        }
    }
    Some(cookie)
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

pub use a11y::{lint as lint_a11y, A11yFinding, Severity as A11ySeverity};
pub use js::{JsRuntime, JsValueResult};
pub use social::{lint as lint_social, Severity as SocialSeverity, SocialFinding, SocialPreview};
pub use seo::{lint as lint_seo, SeoFinding, Severity as SeoSeverity};
pub use layout::compute_layout;
pub use raster::{render_pdf, render_png};
pub use render::render_svg;
pub use webmcp::{
    build_submission as build_webmcp_submission, discover_tools as discover_webmcp_tools,
    lint as lint_webmcp, LintFinding as WebMcpLintFinding, Severity as WebMcpLintSeverity, WebMcpTool,
};

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
    let mut meta = Vec::new();
    collect_meta(&dom.document, &mut meta);
    let text = normalize_text(&root.text);

    Page {
        url: None,
        title,
        text,
        links,
        root,
        meta,
        cookies: Vec::new(),
        source: Some(source.to_string()),
    }
}

/// The subset of `Element`'s fields that come straight from parsed
/// attributes, before layout/children/text are known.
#[derive(Default)]
struct ParsedAttrs {
    tag: String,
    href: Option<String>,
    value: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    tool_name: Option<String>,
    tool_description: Option<String>,
    tool_autosubmit: bool,
    tool_param_description: Option<String>,
    required: bool,
    method: Option<String>,
    alt: Option<String>,
    lang: Option<String>,
}

fn element_from_handle(handle: &Handle) -> Element {
    let attrs = match &handle.data {
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
            let has_attr = |attr_name: &str| -> bool {
                attrs.iter().any(|attr| attr.name.local.as_ref() == attr_name)
            };
            ParsedAttrs {
                tag,
                href: attr(href_attr_name),
                value: attr("value"),
                name: attr("name"),
                kind: attr("type"),
                tool_name: attr("toolname"),
                tool_description: attr("tooldescription"),
                tool_autosubmit: has_attr("toolautosubmit"),
                tool_param_description: attr("toolparamdescription"),
                required: has_attr("required"),
                method: attr("method"),
                alt: attr("alt"),
                lang: attr("lang"),
            }
        }
        NodeData::Document => ParsedAttrs { tag: "document".to_string(), ..Default::default() },
        _ => ParsedAttrs { tag: "text".to_string(), ..Default::default() },
    };

    if matches!(attrs.tag.as_str(), "head" | "script" | "style") {
        return Element {
            tag: attrs.tag,
            text: String::new(),
            href: attrs.href,
            children: Vec::new(),
            layout: None,
            value: attrs.value,
            name: attrs.name,
            kind: attrs.kind,
            tool_name: attrs.tool_name,
            tool_description: attrs.tool_description,
            tool_autosubmit: attrs.tool_autosubmit,
            tool_param_description: attrs.tool_param_description,
            required: attrs.required,
            method: attrs.method,
            alt: attrs.alt,
            lang: attrs.lang,
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
    let value = if attrs.tag == "textarea" && attrs.value.is_none() {
        Some(text.clone())
    } else {
        attrs.value
    };

    Element {
        tag: attrs.tag,
        text,
        href: attrs.href,
        children,
        layout: None,
        value,
        name: attrs.name,
        kind: attrs.kind,
        tool_name: attrs.tool_name,
        tool_description: attrs.tool_description,
        tool_autosubmit: attrs.tool_autosubmit,
        tool_param_description: attrs.tool_param_description,
        required: attrs.required,
        method: attrs.method,
        alt: attrs.alt,
        lang: attrs.lang,
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

fn collect_meta(handle: &Handle, meta: &mut Vec<(String, String)>) {
    if let NodeData::Element { name, attrs, .. } = &handle.data {
        if name.local.as_ref() == "meta" {
            let attrs = attrs.borrow();
            let find = |attr_name: &str| -> Option<String> {
                attrs
                    .iter()
                    .find(|attr| attr.name.local.as_ref() == attr_name)
                    .map(|attr| attr.value.to_string())
            };
            let key = find("name").or_else(|| find("property"));
            if let (Some(key), Some(content)) = (key, find("content")) {
                meta.push((key, content));
            }
        }
    }
    for child in handle.children.borrow().iter() {
        collect_meta(child, meta);
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
    use super::{parse_html, parse_set_cookie, resolve_links, Element};

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

    #[test]
    fn parses_a_simple_cookie_with_no_attributes() {
        let cookie = parse_set_cookie("session=abc123").expect("should parse");
        assert_eq!(cookie.name, "session");
        assert_eq!(cookie.value, "abc123");
        assert!(!cookie.secure);
        assert!(!cookie.http_only);
        assert!(cookie.domain.is_none());
    }

    #[test]
    fn parses_all_common_attributes_and_flags() {
        let cookie = parse_set_cookie(
            "session=abc123; Path=/; Domain=example.com; Secure; HttpOnly; SameSite=Lax",
        )
        .expect("should parse");
        assert_eq!(cookie.name, "session");
        assert_eq!(cookie.value, "abc123");
        assert_eq!(cookie.path.as_deref(), Some("/"));
        assert_eq!(cookie.domain.as_deref(), Some("example.com"));
        assert!(cookie.secure);
        assert!(cookie.http_only);
        assert_eq!(cookie.same_site.as_deref(), Some("Lax"));
    }

    #[test]
    fn value_may_contain_further_equals_signs() {
        let cookie = parse_set_cookie("token=abc=def==; Path=/").expect("should parse");
        assert_eq!(cookie.name, "token");
        assert_eq!(cookie.value, "abc=def==");
    }

    #[test]
    fn rejects_a_header_with_no_name_value_pair() {
        assert!(parse_set_cookie("").is_none());
        assert!(parse_set_cookie("justsometext").is_none());
    }
}
