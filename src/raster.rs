use crate::{render_svg, Page};
use anyhow::{Context, Result};

/// Loads system fonts and makes sure the generic `sans-serif` family (the
/// only one this project's rendered SVG ever asks for) actually resolves to
/// something that got loaded.
///
/// `fontdb` hard-codes `sans-serif` to "Arial" by default, and only
/// overrides that from the *fontconfig* config files (`/etc/fonts/*.conf`)
/// if fontconfig itself is installed. This project's own Docker image
/// installs `fonts-dejavu-core` (font files) but not the `fontconfig`
/// package (the config that would tell fontdb "sans-serif means DejaVu
/// Sans here") — so without this fallback, `sans-serif` resolves to a font
/// that was never loaded, and every text node silently fails to render: no
/// error, just a blank image. If the generic family doesn't resolve, fall
/// back to whatever font actually got loaded.
fn load_fonts(fontdb: &mut usvg::fontdb::Database) {
    fontdb.load_system_fonts();
    let resolves = fontdb
        .query(&usvg::fontdb::Query {
            families: &[usvg::fontdb::Family::SansSerif],
            ..Default::default()
        })
        .is_some();
    if !resolves {
        let fallback_family = fontdb
            .faces()
            .next()
            .and_then(|face| face.families.first().map(|(name, _)| name.clone()));
        if let Some(family) = fallback_family {
            fontdb.set_sans_serif_family(family);
        }
    }
}

/// Rasterizes the page's SVG rendering (see `render_svg`) to PNG bytes at
/// the given viewport width.
///
/// This loads whatever fonts are installed on the *system running this
/// binary* (via `fontdb`'s `load_system_fonts`) — there is no bundled font.
/// On a machine or container with no fonts installed, text silently fails
/// to render and the PNG comes back with the text missing (headings,
/// paragraphs, links — everything this project draws is text). See the
/// README's "Renderizado PNG/PDF" section.
pub fn render_png(page: &Page, width: u32) -> Result<Vec<u8>> {
    let svg = render_svg(page, width);
    let mut options = usvg::Options::default();
    load_fonts(options.fontdb_mut());
    let tree = usvg::Tree::from_str(&svg, &options).context("failed to parse rendered SVG")?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .context("rendered page has invalid pixel dimensions")?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap.encode_png().context("failed to encode PNG")
}

/// Wraps `render_png`'s output as a single-page PDF, at 96 DPI so the page
/// is sized to exactly fit the rasterized image.
///
/// This is a raster image embedded in a PDF page, not a vector PDF — the
/// text is not selectable or searchable. A true vector PDF would need its
/// own font-embedding pipeline independent of this raster path; see the
/// README for why that's out of scope here.
pub fn render_pdf(page: &Page, width: u32) -> Result<Vec<u8>> {
    const DPI: f32 = 96.0;

    let png_bytes = render_png(page, width)?;
    let decoder = printpdf::image_crate::codecs::png::PngDecoder::new(std::io::Cursor::new(
        png_bytes.as_slice(),
    ))
    .context("failed to decode the rasterized PNG")?;
    let image =
        printpdf::Image::try_from(decoder).context("failed to load the PNG into a PDF image")?;

    let width_mm = image.image.width.0 as f32 / DPI * 25.4;
    let height_mm = image.image.height.0 as f32 / DPI * 25.4;
    let (doc, page_index, layer_index) = printpdf::PdfDocument::new(
        "kite-lite",
        printpdf::Mm(width_mm),
        printpdf::Mm(height_mm),
        "Layer 1",
    );
    let layer = doc.get_page(page_index).get_layer(layer_index);
    image.add_to_layer(
        layer,
        printpdf::ImageTransform {
            dpi: Some(DPI),
            ..Default::default()
        },
    );
    doc.save_to_bytes().context("failed to save PDF")
}

#[cfg(test)]
mod tests {
    use super::{render_pdf, render_png};
    use crate::parse_html;

    #[test]
    fn renders_a_page_to_png_bytes() {
        let page = parse_html("<h1>Hello</h1>");
        let png = render_png(&page, 320).expect("PNG rendering failed");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "missing PNG signature");
    }

    #[test]
    fn rendered_png_actually_draws_text_pixels() {
        // A valid PNG signature isn't enough to prove text rendered: an
        // unresolved font family fails silently and produces a
        // structurally valid but entirely blank (all-white) image. This
        // guards against that regression by checking real pixel content.
        let page = parse_html("<h1>Hello</h1>");
        let png = render_png(&page, 320).expect("PNG rendering failed");
        let pixmap = tiny_skia::Pixmap::decode_png(&png).expect("failed to decode rendered PNG");
        let has_non_white_pixel = pixmap
            .pixels()
            .iter()
            .any(|pixel| pixel.red() != 255 || pixel.green() != 255 || pixel.blue() != 255);
        assert!(has_non_white_pixel, "rendered PNG is entirely blank/white");
    }

    #[test]
    fn renders_a_page_to_pdf_bytes() {
        let page = parse_html("<h1>Hello</h1>");
        let pdf = render_pdf(&page, 320).expect("PDF rendering failed");
        assert_eq!(&pdf[..5], b"%PDF-", "missing PDF signature");
    }
}
