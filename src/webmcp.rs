//! Declarative WebMCP support: reads the `toolname`/`tooldescription`/
//! `toolparamdescription`/`toolautosubmit`/`required` HTML attributes a page
//! author puts on a `<form>` and its fields — see
//! <https://github.com/webmachinelearning/webmcp/blob/main/declarative-api-explainer.md>
//! — and turns that into an MCP-shaped tool definition plus a way to submit
//! it. There's no live JS execution here: only what's on the parsed HTML
//! (the imperative `navigator.modelContext` API is out of scope).

use crate::{Element, Page};
use serde_json::{json, Map, Value};

#[derive(Debug, Clone)]
pub struct WebMcpTool {
    pub name: String,
    pub description: String,
    pub autosubmit: bool,
    pub input_schema: Value,
}

/// Finds every `<form toolname="...">` in `page` and describes it as a tool.
pub fn discover_tools(page: &Page) -> Vec<WebMcpTool> {
    let mut tools = Vec::new();
    collect_tools(&page.root, &mut tools);
    tools
}

fn collect_tools(element: &Element, tools: &mut Vec<WebMcpTool>) {
    if element.tag == "form" {
        if let Some(name) = &element.tool_name {
            tools.push(build_tool(element, name));
        }
    }
    for child in &element.children {
        collect_tools(child, tools);
    }
}

fn build_tool(form: &Element, name: &str) -> WebMcpTool {
    let mut properties = Map::new();
    let mut required = Vec::new();
    collect_field_schemas(form, &mut properties, &mut required);
    WebMcpTool {
        name: name.to_string(),
        description: form.tool_description.clone().unwrap_or_default(),
        autosubmit: form.tool_autosubmit,
        input_schema: json!({
            "type": "object",
            "properties": Value::Object(properties),
            "required": required,
        }),
    }
}

fn collect_field_schemas(element: &Element, properties: &mut Map<String, Value>, required: &mut Vec<String>) {
    if let Some(field_name) = field_name(element) {
        let mut schema = field_type_schema(element);
        if let Some(description) = &element.tool_param_description {
            schema["description"] = json!(description);
        }
        properties.insert(field_name.clone(), schema);
        if element.required {
            required.push(field_name);
        }
    }
    for child in &element.children {
        collect_field_schemas(child, properties, required);
    }
}

fn field_name(element: &Element) -> Option<String> {
    matches!(element.tag.as_str(), "input" | "textarea" | "select")
        .then(|| element.name.clone())
        .flatten()
}

fn field_type_schema(element: &Element) -> Value {
    if element.tag == "select" {
        let options: Vec<String> = option_values(element).collect();
        return json!({"type": "string", "enum": options});
    }
    match element.kind.as_deref() {
        Some("checkbox") => json!({"type": "boolean"}),
        Some("number") => json!({"type": "number"}),
        _ => json!({"type": "string"}),
    }
}

fn option_values(select: &Element) -> impl Iterator<Item = String> + '_ {
    select
        .children
        .iter()
        .filter(|child| child.tag == "option")
        .map(|option| option.value.clone().unwrap_or_else(|| option.text.clone()))
}

/// Finds the form registered as `tool_name` and builds the GET query string
/// it would submit, filling each field from `arguments` (falling back to the
/// field's current DOM value when an argument is missing). Mirrors the same
/// GET-only limitation as CDP's click-driven form submit: there's no request
/// body, so a form's real `method="post"` is not honored.
pub fn build_submission(page: &Page, tool_name: &str, arguments: &Value) -> Option<(String, String)> {
    let form = find_form(&page.root, tool_name)?;
    let action_url = form.href.clone().or_else(|| page.url.clone()).unwrap_or_default();
    let mut fields = Vec::new();
    collect_arg_fields(form, arguments, &mut fields);
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in &fields {
        serializer.append_pair(name, value);
    }
    Some((action_url, serializer.finish()))
}

fn find_form<'a>(element: &'a Element, tool_name: &str) -> Option<&'a Element> {
    if element.tag == "form" && element.tool_name.as_deref() == Some(tool_name) {
        return Some(element);
    }
    element.children.iter().find_map(|child| find_form(child, tool_name))
}

fn collect_arg_fields(element: &Element, arguments: &Value, fields: &mut Vec<(String, String)>) {
    if let Some(name) = field_name(element) {
        let value = arguments
            .get(&name)
            .map(argument_to_string)
            .unwrap_or_else(|| default_field_value(element));
        fields.push((name, value));
    }
    for child in &element.children {
        collect_arg_fields(child, arguments, fields);
    }
}

fn argument_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn default_field_value(element: &Element) -> String {
    if element.tag == "select" {
        option_values(element).next().unwrap_or_default()
    } else {
        element.value.clone().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_html;

    #[test]
    fn discovers_a_form_with_toolname_as_a_tool() {
        let page = parse_html(
            r#"<form toolname="search-cars" tooldescription="Search for a car">
                 <input type="text" name="make" required toolparamdescription="The make">
                 <input type="text" name="model">
                 <button type="submit">Search</button>
               </form>"#,
        );
        let tools = discover_tools(&page);
        assert_eq!(tools.len(), 1);
        let tool = &tools[0];
        assert_eq!(tool.name, "search-cars");
        assert_eq!(tool.description, "Search for a car");
        assert!(!tool.autosubmit);
        assert_eq!(
            tool.input_schema["properties"]["make"]["description"],
            "The make"
        );
        assert_eq!(tool.input_schema["required"], json!(["make"]));
        assert!(tool.input_schema["properties"]["model"]["description"].is_null());
    }

    #[test]
    fn ignores_forms_without_a_toolname() {
        let page = parse_html(r#"<form action="/search"><input name="q"></form>"#);
        assert!(discover_tools(&page).is_empty());
    }

    #[test]
    fn select_fields_become_a_string_enum() {
        let page = parse_html(
            r#"<form toolname="route" tooldescription="Route a request">
                 <select name="team">
                   <option value="a">Team A</option>
                   <option value="b">Team B</option>
                 </select>
               </form>"#,
        );
        let tool = &discover_tools(&page)[0];
        assert_eq!(
            tool.input_schema["properties"]["team"]["enum"],
            json!(["a", "b"])
        );
    }

    #[test]
    fn autosubmit_attribute_is_reported() {
        let page = parse_html(
            r#"<form toolname="go" tooldescription="Go" toolautosubmit><input name="q"></form>"#,
        );
        assert!(discover_tools(&page)[0].autosubmit);
    }

    #[test]
    fn build_submission_uses_arguments_and_falls_back_to_dom_values() {
        let mut page = parse_html(
            r#"<form toolname="search" tooldescription="Search" action="/search">
                 <input name="q">
                 <input name="lang" value="en">
               </form>"#,
        );
        crate::resolve_links(&mut page, "https://example.com/");
        let (action_url, query) =
            build_submission(&page, "search", &json!({"q": "rust mcp"})).expect("tool not found");
        assert_eq!(action_url, "https://example.com/search");
        assert_eq!(query, "q=rust+mcp&lang=en");
    }

    #[test]
    fn build_submission_returns_none_for_an_unknown_tool() {
        let page = parse_html(r#"<form toolname="a" tooldescription="A"></form>"#);
        assert!(build_submission(&page, "b", &json!({})).is_none());
    }
}
