//! Declarative WebMCP support: reads the `toolname`/`tooldescription`/
//! `toolparamdescription`/`toolautosubmit`/`required` HTML attributes a page
//! author puts on a `<form>` and its fields — see
//! <https://github.com/webmachinelearning/webmcp/blob/main/declarative-api-explainer.md>
//! — and turns that into an MCP-shaped tool definition plus a way to submit
//! it. There's no live JS execution here: only what's on the parsed HTML
//! (the imperative `navigator.modelContext` API is out of scope).

use crate::{Element, Page};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

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

fn collect_field_schemas(
    element: &Element,
    properties: &mut Map<String, Value>,
    required: &mut Vec<String>,
) {
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
pub fn build_submission(
    page: &Page,
    tool_name: &str,
    arguments: &Value,
) -> Option<(String, String)> {
    let form = find_form(&page.root, tool_name)?;
    let action_url = form
        .href
        .clone()
        .or_else(|| page.url.clone())
        .unwrap_or_default();
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
    element
        .children
        .iter()
        .find_map(|child| find_form(child, tool_name))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Breaks the tool for any agent: missing/duplicate name, no description.
    Error,
    /// Works, but likely not what the author intended.
    Warning,
    /// Not a spec violation — a kite-lite-specific caveat or a style nit.
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct LintFinding {
    pub severity: Severity,
    pub tool: String,
    pub message: String,
}

/// Checks every `<form toolname="...">` on `page` against a handful of
/// practical rules — see the individual `push_*` calls below for what's
/// checked and why. Only forms that opt in with `toolname` are inspected;
/// a plain form is not an error.
pub fn lint(page: &Page) -> Vec<LintFinding> {
    let mut forms = Vec::new();
    collect_tool_forms(&page.root, &mut forms);

    let mut occurrences: HashMap<&str, u32> = HashMap::new();
    for form in &forms {
        let name = form.tool_name.as_deref().unwrap_or_default();
        *occurrences.entry(name).or_insert(0) += 1;
    }

    let mut findings = Vec::new();
    for form in &forms {
        let name = form.tool_name.as_deref().unwrap_or_default();
        lint_form(form, name, occurrences[name], &mut findings);
    }
    findings
}

fn collect_tool_forms<'a>(element: &'a Element, forms: &mut Vec<&'a Element>) {
    if element.tag == "form" && element.tool_name.is_some() {
        forms.push(element);
    }
    for child in &element.children {
        collect_tool_forms(child, forms);
    }
}

fn add_finding(
    findings: &mut Vec<LintFinding>,
    severity: Severity,
    tool: &str,
    message: impl Into<String>,
) {
    findings.push(LintFinding {
        severity,
        tool: tool.to_string(),
        message: message.into(),
    });
}

fn lint_form(form: &Element, name: &str, occurrences: u32, findings: &mut Vec<LintFinding>) {
    if name.trim().is_empty() {
        add_finding(findings, Severity::Error, name, "toolname esta vacio");
    } else if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        add_finding(
            findings,
            Severity::Warning,
            name,
            format!("toolname '{name}' tiene caracteres fuera de [A-Za-z0-9_-]; algunos backends de tool-calling lo rechazan"),
        );
    }

    if occurrences > 1 {
        add_finding(
            findings,
            Severity::Error,
            name,
            format!("hay {occurrences} formularios con toolname '{name}' en la pagina: un agente no puede distinguir cual invocar"),
        );
    }

    if form
        .tool_description
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        add_finding(
            findings,
            Severity::Error,
            name,
            "falta tooldescription (obligatorio: sin descripcion el agente no sabe cuando usar la tool)",
        );
    }

    if form.href.is_none() {
        add_finding(
            findings,
            Severity::Warning,
            name,
            "el formulario no tiene 'action': el submit apuntara a la URL de la propia pagina, probablemente no es lo que se busca",
        );
    }

    if form
        .method
        .as_deref()
        .is_some_and(|m| m.eq_ignore_ascii_case("post"))
    {
        add_finding(
            findings,
            Severity::Info,
            name,
            "method=\"post\": kite-lite solo puede simular un submit GET (browser_call_tool no reflejara el comportamiento real de este formulario) — probalo tambien en un navegador con WebMCP nativo",
        );
    }

    lint_fields(form, name, findings);
}

fn lint_fields(element: &Element, tool_name: &str, findings: &mut Vec<LintFinding>) {
    if matches!(element.tag.as_str(), "input" | "textarea" | "select") {
        match &element.name {
            None => add_finding(
                findings,
                Severity::Warning,
                tool_name,
                format!("un <{}> del formulario no tiene 'name': queda fuera del schema y el agente no podra completarlo", element.tag),
            ),
            Some(field_name) => {
                if element.tool_param_description.is_none() {
                    add_finding(
                        findings,
                        Severity::Info,
                        tool_name,
                        format!("el campo '{field_name}' no tiene toolparamdescription; no es obligatorio pero ayuda al agente a completarlo bien"),
                    );
                }
                if element.tag == "select" && option_values(element).next().is_none() {
                    add_finding(
                        findings,
                        Severity::Warning,
                        tool_name,
                        format!("<select name=\"{field_name}\"> no tiene ninguna <option>: el schema generado tendria un enum vacio"),
                    );
                }
            }
        }
    }
    for child in &element.children {
        lint_fields(child, tool_name, findings);
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

    fn messages(findings: &[LintFinding], severity: Severity) -> Vec<&str> {
        findings
            .iter()
            .filter(|f| f.severity == severity)
            .map(|f| f.message.as_str())
            .collect()
    }

    #[test]
    fn a_well_formed_tool_produces_no_findings() {
        let page = parse_html(
            r#"<form toolname="search-cars" tooldescription="Search for a car" action="/search">
                 <input type="text" name="make" toolparamdescription="The make">
               </form>"#,
        );
        assert!(lint(&page).is_empty());
    }

    #[test]
    fn missing_tooldescription_is_an_error() {
        let page = parse_html(r#"<form toolname="go" action="/x"><input name="q"></form>"#);
        let findings = lint(&page);
        assert!(messages(&findings, Severity::Error)
            .iter()
            .any(|m| m.contains("tooldescription")));
    }

    #[test]
    fn duplicate_toolnames_are_an_error() {
        let page = parse_html(
            r#"<form toolname="go" tooldescription="Go" action="/a"></form>
               <form toolname="go" tooldescription="Go again" action="/b"></form>"#,
        );
        let findings = lint(&page);
        assert!(messages(&findings, Severity::Error)
            .iter()
            .any(|m| m.contains("2 formularios")));
    }

    #[test]
    fn field_without_name_is_a_warning() {
        let page = parse_html(
            r#"<form toolname="go" tooldescription="Go" action="/x"><input type="text"></form>"#,
        );
        let findings = lint(&page);
        assert!(messages(&findings, Severity::Warning)
            .iter()
            .any(|m| m.contains("no tiene 'name'")));
    }

    #[test]
    fn empty_select_is_a_warning() {
        let page = parse_html(
            r#"<form toolname="go" tooldescription="Go" action="/x"><select name="team"></select></form>"#,
        );
        let findings = lint(&page);
        assert!(messages(&findings, Severity::Warning)
            .iter()
            .any(|m| m.contains("enum vacio")));
    }

    #[test]
    fn missing_action_is_a_warning() {
        let page = parse_html(r#"<form toolname="go" tooldescription="Go"></form>"#);
        let findings = lint(&page);
        assert!(messages(&findings, Severity::Warning)
            .iter()
            .any(|m| m.contains("'action'")));
    }

    #[test]
    fn post_method_is_an_info_note_not_an_error() {
        let page = parse_html(
            r#"<form toolname="go" tooldescription="Go" action="/x" method="post"></form>"#,
        );
        let findings = lint(&page);
        assert!(messages(&findings, Severity::Info)
            .iter()
            .any(|m| m.contains("method=\"post\"")));
        assert!(findings
            .iter()
            .all(|f| f.severity != Severity::Error || !f.message.contains("post")));
    }

    #[test]
    fn field_without_toolparamdescription_is_an_info_note() {
        let page = parse_html(
            r#"<form toolname="go" tooldescription="Go" action="/x"><input name="q"></form>"#,
        );
        let findings = lint(&page);
        assert!(messages(&findings, Severity::Info)
            .iter()
            .any(|m| m.contains("toolparamdescription")));
    }

    #[test]
    fn toolname_with_spaces_is_a_warning() {
        let page =
            parse_html(r#"<form toolname="go now" tooldescription="Go" action="/x"></form>"#);
        let findings = lint(&page);
        assert!(messages(&findings, Severity::Warning)
            .iter()
            .any(|m| m.contains("caracteres fuera de")));
    }
}
