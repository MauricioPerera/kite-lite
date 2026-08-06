use crate::layout::{compute_layout, line_height, tag_style, wrap_text};
use crate::{Element, Page};

const PAGE_PADDING: f64 = 20.0;
const TITLE_FONT_SIZE: f64 = 26.0;

pub fn render_svg(page: &Page, width: u32) -> String {
    let width = width.max(320);
    let content_width = (f64::from(width) - PAGE_PADDING * 2.0).max(80.0);

    let mut layout_page = page.clone();
    compute_layout(&mut layout_page, content_width);

    let mut draws: Vec<(f64, f64, bool, String)> = Vec::new();
    let mut content_top = PAGE_PADDING;

    if let Some(title) = &page.title {
        content_top += line_height(TITLE_FONT_SIZE);
        draws.push((content_top, TITLE_FONT_SIZE, true, title.clone()));
    }

    collect_rendered_lines(&layout_page.root, content_top, content_width, &mut draws);

    let content_height = layout_page.root.layout.map_or(0.0, |layout| layout.height);
    let height = (content_top + content_height + PAGE_PADDING).max(80.0) as u32;

    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
<rect width="100%" height="100%" fill="#ffffff"/>
<style>text {{ font-family: system-ui, sans-serif; fill: #202124; }} .heading {{ font-weight: 700; }} .muted {{ fill: #5f6368; }}</style>
"##
    );

    for (y, size, bold, text) in &draws {
        let class = if *bold { "heading" } else { "muted" };
        svg.push_str(&format!(
            r#"<text x="{PAGE_PADDING}" y="{y:.0}" font-size="{size:.0}px" class="{class}">{}</text>
"#,
            escape_xml(text)
        ));
    }
    svg.push_str("</svg>\n");
    svg
}

/// Walks the already-laid-out tree, emitting one draw instruction per
/// wrapped line of every leaf element's text. `y_offset` shifts every
/// element's own (layout-relative) `y` down by however much space the page
/// title reserved above the document, since the title isn't part of the DOM
/// tree that `compute_layout` positioned.
fn collect_rendered_lines(
    element: &Element,
    y_offset: f64,
    available_width: f64,
    output: &mut Vec<(f64, f64, bool, String)>,
) {
    if element.children.is_empty() {
        if let (Some(layout), false) = (&element.layout, element.text.is_empty()) {
            let style = tag_style(&element.tag);
            let line_h = line_height(style.font_size);
            for (index, line) in wrap_text(&element.text, available_width, style.font_size)
                .into_iter()
                .enumerate()
            {
                let baseline = y_offset + layout.y + line_h * (index as f64 + 1.0);
                output.push((baseline, style.font_size, style.bold, line));
            }
        }
        return;
    }
    for child in &element.children {
        collect_rendered_lines(child, y_offset, available_width, output);
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

    #[test]
    fn wraps_a_long_paragraph_into_multiple_lines() {
        let long_text = "word ".repeat(60);
        let page = parse_html(&format!("<p>{long_text}</p>"));
        let svg = render_svg(&page, 400);
        let text_elements = svg.matches("<text").count();
        assert!(
            text_elements > 1,
            "expected the paragraph to wrap across multiple <text> lines, got {text_elements}"
        );
    }

    #[test]
    fn heading_and_paragraph_do_not_overlap_vertically() {
        let page = parse_html("<h1>Title</h1><p>Body</p>");
        let svg = render_svg(&page, 640);
        let ys: Vec<f64> = svg
            .lines()
            .filter_map(|line| {
                let start = line.find("y=\"")? + 3;
                let rest = &line[start..];
                let end = rest.find('"')?;
                rest[..end].parse::<f64>().ok()
            })
            .collect();
        assert_eq!(ys.len(), 2, "expected exactly two text lines, got {ys:?}");
        assert!(ys[1] > ys[0], "paragraph should be drawn below the heading");
    }
}
