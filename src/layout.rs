use crate::{Element, Layout, Page};

const LINE_HEIGHT_RATIO: f64 = 1.3;
const AVG_CHAR_WIDTH_RATIO: f64 = 0.55;

pub(crate) struct TagStyle {
    pub font_size: f64,
    pub margin: f64,
    pub bold: bool,
}

/// A minimal stand-in for a default browser stylesheet: a fixed style per
/// tag name, not anything parsed from the page (there is no `<style>` or
/// `style=""` support). `margin` applies to both the top and bottom of a
/// block, mirroring how most default UA stylesheets use the same value on
/// both sides.
pub(crate) fn tag_style(tag: &str) -> TagStyle {
    match tag {
        "h1" => TagStyle { font_size: 32.0, margin: 24.0, bold: true },
        "h2" => TagStyle { font_size: 24.0, margin: 20.0, bold: true },
        "h3" => TagStyle { font_size: 19.0, margin: 18.0, bold: true },
        "h4" => TagStyle { font_size: 16.0, margin: 16.0, bold: true },
        "h5" => TagStyle { font_size: 13.0, margin: 14.0, bold: true },
        "h6" => TagStyle { font_size: 11.0, margin: 14.0, bold: true },
        "p" | "blockquote" | "ul" | "ol" => TagStyle { font_size: 16.0, margin: 16.0, bold: false },
        "li" => TagStyle { font_size: 16.0, margin: 4.0, bold: false },
        "strong" | "b" => TagStyle { font_size: 16.0, margin: 0.0, bold: true },
        _ => TagStyle { font_size: 16.0, margin: 0.0, bold: false },
    }
}

pub(crate) fn line_height(font_size: f64) -> f64 {
    font_size * LINE_HEIGHT_RATIO
}

/// Greedily wraps `text` into lines that fit within `available_width`,
/// estimating each character's width as a fixed fraction of `font_size`
/// (there's no real font metrics or text shaping here). Wraps on
/// whitespace only — a single word longer than the available width is not
/// hyphenated or broken, it just overflows its line.
pub(crate) fn wrap_text(text: &str, available_width: f64, font_size: f64) -> Vec<String> {
    let avg_char_width = (font_size * AVG_CHAR_WIDTH_RATIO).max(1.0);
    let max_chars = (available_width / avg_char_width).floor().max(1.0) as usize;

    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate_len = if current.is_empty() {
            word.chars().count()
        } else {
            current.chars().count() + 1 + word.chars().count()
        };
        if candidate_len > max_chars && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Computes a minimal block layout for `page.root` and every descendant, as
/// if laid out in a `viewport_width`-wide viewport, storing the result in
/// each element's `layout` field.
///
/// This is deliberately not a CSS engine: every element with children
/// simply stacks its children vertically at full viewport width (there is
/// no true inline flow, floats, or positioning). It can't be otherwise
/// without changing the parser — loose text sitting next to a child element
/// (e.g. `<p>Hello <a href="/x">link</a></p>`) is merged into the parent's
/// `text` field rather than kept as a separate sibling node, so that loose
/// text has nowhere to be laid out once the parent has an element child;
/// only leaf elements' own text gets wrapped and sized here. `x` is always
/// `0.0` for the same reason: there's no horizontal layout dimension.
pub fn compute_layout(page: &mut Page, viewport_width: f64) {
    layout_element(&mut page.root, 0.0, viewport_width.max(1.0));
}

/// Lays out `element` with its top-left corner's `y` at `y`, and returns
/// the total vertical space it consumed, including its own margins, so a
/// parent can advance its stacking cursor by that amount.
fn layout_element(element: &mut Element, y: f64, available_width: f64) -> f64 {
    let style = tag_style(&element.tag);
    let top = y + style.margin;

    let content_height = if element.children.is_empty() {
        if element.text.is_empty() {
            0.0
        } else {
            let lines = wrap_text(&element.text, available_width, style.font_size);
            lines.len() as f64 * line_height(style.font_size)
        }
    } else {
        let mut cursor = top;
        for child in &mut element.children {
            cursor += layout_element(child, cursor, available_width);
        }
        cursor - top
    };

    element.layout = Some(Layout {
        x: 0.0,
        y: top,
        width: available_width,
        height: content_height,
    });

    content_height + style.margin
}

#[cfg(test)]
mod tests {
    use super::{compute_layout, wrap_text};
    use crate::parse_html;

    #[test]
    fn wraps_text_on_word_boundaries() {
        let lines = wrap_text("one two three four", 60.0, 16.0);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| !line.is_empty()));
        assert_eq!(lines.join(" "), "one two three four");
    }

    #[test]
    fn stacks_headings_and_paragraphs_vertically_with_margins() {
        let mut page = parse_html("<h1>Title</h1><p>Body text</p>");
        compute_layout(&mut page, 800.0);

        let html = &page.root.children[0];
        let h1 = &html.children[1].children[0];
        let p = &html.children[1].children[1];

        let h1_layout = h1.layout.expect("h1 should have a computed layout");
        let p_layout = p.layout.expect("p should have a computed layout");

        assert!(h1_layout.y < p_layout.y, "p should sit below h1");
        assert!(
            p_layout.y >= h1_layout.y + h1_layout.height,
            "p's top should not overlap h1's box"
        );
        assert_eq!(h1_layout.width, 800.0);
    }

    #[test]
    fn empty_page_has_zero_root_height_contribution_from_empty_children() {
        let mut page = parse_html("");
        compute_layout(&mut page, 500.0);
        assert_eq!(page.root.layout.unwrap().height, 0.0);
    }
}
