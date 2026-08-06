use crate::{Element, Page};
use anyhow::{anyhow, Result};
use boa_engine::{Context, Source};

/// A short-lived JavaScript context with no host bindings.
///
/// Each call creates a fresh Boa context, so scripts cannot share globals,
/// access the filesystem, or perform network requests through this runtime.
pub struct JsRuntime {
    max_source_bytes: usize,
    recursion_limit: usize,
    stack_size_limit: usize,
}

pub type JsValueResult = String;

impl Default for JsRuntime {
    fn default() -> Self {
        Self {
            max_source_bytes: 64 * 1024,
            recursion_limit: 256,
            stack_size_limit: 16 * 1024,
        }
    }
}

impl JsRuntime {
    pub fn evaluate(&self, source: &str) -> Result<JsValueResult> {
        self.evaluate_in_context("", source)
    }

    pub fn evaluate_page(&self, page: &Page, source: &str) -> Result<JsValueResult> {
        let title = serde_json::to_string(page.title.as_deref().unwrap_or(""))?;
        let body = serde_json::to_string(&page.text)?;
        let selectors = serde_json::to_string(&selector_texts(&page.root))?;
        let prelude = format!(
            "const document = {{ title: {title}, body: {{ innerText: {body} }}, querySelector: (selector) => ({{ innerText: ({selectors})[selector] || '' }}) }};"
        );
        self.evaluate_in_context(&prelude, source)
    }

    fn evaluate_in_context(&self, prelude: &str, source: &str) -> Result<JsValueResult> {
        if source.len() > self.max_source_bytes {
            return Err(anyhow!(
                "script exceeds the {} byte safety limit",
                self.max_source_bytes
            ));
        }

        let mut context = Context::default();
        context
            .runtime_limits_mut()
            .set_recursion_limit(self.recursion_limit);
        context
            .runtime_limits_mut()
            .set_stack_size_limit(self.stack_size_limit);
        let code = format!("{prelude}\n{source}");
        let value = context
            .eval(Source::from_bytes(&code))
            .map_err(|error| anyhow!("JavaScript error: {error}"))?;
        Ok(value
            .to_string(&mut context)
            .map_err(|error| anyhow!("JavaScript conversion error: {error}"))?
            .to_std_string_escaped())
    }
}

fn selector_texts(root: &Element) -> std::collections::BTreeMap<String, String> {
    let mut values = std::collections::BTreeMap::new();
    collect_selector_texts(root, &mut values);
    values
}

fn collect_selector_texts(
    element: &Element,
    values: &mut std::collections::BTreeMap<String, String>,
) {
    if !element.text.is_empty()
        && matches!(
            element.tag.as_str(),
            "h1" | "h2" | "h3" | "p" | "a" | "button"
        )
    {
        values
            .entry(element.tag.clone())
            .or_insert_with(|| element.text.clone());
    }
    for child in &element.children {
        collect_selector_texts(child, values);
    }
}

#[cfg(test)]
mod tests {
    use super::JsRuntime;

    #[test]
    fn evaluates_in_a_fresh_context() {
        let runtime = JsRuntime::default();
        assert_eq!(runtime.evaluate("1 + 2").unwrap(), "3");
        assert!(runtime.evaluate("typeof process").is_ok());
        assert!(runtime.evaluate("require('fs')").is_err());
        assert!(runtime.evaluate("(function f(){ return f(); })()").is_err());
    }

    #[test]
    fn exposes_only_a_small_page_snapshot() {
        let page = crate::parse_html("<title>Test</title><h1>Hello</h1><p>Body</p>");
        let runtime = JsRuntime::default();
        assert_eq!(
            runtime.evaluate_page(&page, "document.title").unwrap(),
            "Test"
        );
        assert_eq!(
            runtime
                .evaluate_page(&page, "document.querySelector('h1').innerText")
                .unwrap(),
            "Hello"
        );
        assert!(runtime
            .evaluate_page(&page, "fetch('https://example.com')")
            .is_err());
    }
}
