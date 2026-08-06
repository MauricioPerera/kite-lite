use crate::{Element, Page};

pub fn render_svg(page: &Page, width: u32) -> String {
    let width = width.max(320);
    let mut lines = Vec::new();
    if let Some(title) = &page.title {
        lines.push((title.clone(), 26, true));
    }
    collect_visible_lines(&page.root, &mut lines);

    let line_height = 28;
    let height = (lines.len() as u32 * line_height + 32).max(80);
    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
<rect width="100%" height="100%" fill="#ffffff"/>
<style>text {{ font-family: system-ui, sans-serif; fill: #202124; }} .heading {{ font-weight: 700; }} .muted {{ fill: #5f6368; }}</style>
"##
    );

    for (index, (text, size, heading)) in lines.iter().enumerate() {
        let y = 28 + index as u32 * line_height;
        let class = if *heading { "heading" } else { "muted" };
        svg.push_str(&format!(
            r#"<text x="20" y="{y}" font-size="{size}px" class="{class}">{}</text>
"#,
            escape_xml(text)
        ));
    }
    svg.push_str("</svg>\n");
    svg
}

fn collect_visible_lines(element: &Element, lines: &mut Vec<(String, u32, bool)>) {
    let tag = element.tag.as_str();
    let heading = matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6");
    let visible = heading || matches!(tag, "p" | "li" | "a" | "button" | "blockquote");
    if visible && !element.text.is_empty() {
        let size = if heading { 22 } else { 16 };
        lines.push((element.text.clone(), size, heading));
    }
    for child in &element.children {
        collect_visible_lines(child, lines);
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::render_svg;
    use crate::parse_html;

    #[test]
    fn renders_safe_svg_text() {
        let page = parse_html("<h1>Hello & goodbye</h1>");
        let svg = render_svg(&page, 640);
        assert!(svg.contains("Hello &amp; goodbye"));
        assert!(svg.starts_with("<svg"));
    }
}
