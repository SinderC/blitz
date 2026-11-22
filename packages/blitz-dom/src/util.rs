use crate::node::{Node, NodeData};
use color::{AlphaColor, Srgb};
use keyboard_types::Modifiers;
use std::borrow::Cow;
use style::color::AbsoluteColor;

#[cfg(target_os = "macos")]
pub(crate) const ACTION_MOD: Modifiers = Modifiers::META;
#[cfg(not(target_os = "macos"))]
pub(crate) const ACTION_MOD: Modifiers = Modifiers::CONTROL;

pub type Color = AlphaColor<Srgb>;

/// Decode raw font bytes, decompressing WOFF/WOFF2 if the `woff` feature is enabled.
/// Returns the original slice unchanged for TTF/OTF input, and also on decompression
/// failure. With the `woff` feature disabled, all input passes through unchanged.
pub fn decode_font_bytes(bytes: &[u8]) -> Cow<'_, [u8]> {
    if bytes.len() < 4 {
        return Cow::Borrowed(bytes);
    }
    match &bytes[0..4] {
        #[cfg(feature = "woff")]
        b"wOFF" => wuff::decompress_woff1(bytes)
            .map(Cow::Owned)
            .unwrap_or_else(|_| {
                #[cfg(feature = "tracing")]
                tracing::warn!("Failed to decompress woff1 font");
                Cow::Borrowed(bytes)
            }),
        #[cfg(feature = "woff")]
        b"wOF2" => wuff::decompress_woff2(bytes)
            .map(Cow::Owned)
            .unwrap_or_else(|_| {
                #[cfg(feature = "tracing")]
                tracing::warn!("Failed to decompress woff2 font");
                Cow::Borrowed(bytes)
            }),
        _ => Cow::Borrowed(bytes),
    }
}

#[cfg(feature = "svg")]
use std::sync::{Arc, LazyLock};
#[cfg(feature = "svg")]
use usvg::fontdb;
#[cfg(feature = "svg")]
pub(crate) static FONT_DB: LazyLock<Arc<fontdb::Database>> = LazyLock::new(|| {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    Arc::new(db)
});

/// Which kind of CSS image layer list (`background-image` or `mask-image`) to
/// flush from style to dedicated storage on the node.
#[derive(Clone, Copy, Debug)]
pub enum ImageLayerKind {
    Background,
    Mask,
}

impl ImageLayerKind {
    pub fn image_type(self, idx: usize) -> ImageType {
        match self {
            Self::Background => ImageType::Background(idx),
            Self::Mask => ImageType::Mask(idx),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ImageType {
    Image,
    Background(usize),
    Mask(usize),
}

/// A point
#[derive(Clone, Debug, Copy, Eq, PartialEq)]
pub struct Point<T> {
    /// The x coordinate
    pub x: T,
    /// The y coordinate
    pub y: T,
}

impl Point<f64> {
    pub const ZERO: Self = Point { x: 0.0, y: 0.0 };
}

// Debug print an RcDom
pub fn walk_tree(indent: usize, node: &Node) {
    // Skip all-whitespace text nodes entirely
    if let NodeData::Text(data) = &node.data {
        if data.content.chars().all(|c| c.is_ascii_whitespace()) {
            return;
        }
    }

    print!("{}", " ".repeat(indent));
    let id = node.id;
    match &node.data {
        NodeData::Document(_) => println!("#Document {id}"),

        NodeData::Text(data) => {
            if data.content.chars().all(|c| c.is_ascii_whitespace()) {
                println!("{id} #text: <whitespace>");
            } else {
                let content = data.content.trim();
                if content.len() > 10 {
                    println!(
                        "#text {id}: {}...",
                        content
                            .split_at(content.char_indices().take(10).last().unwrap().0)
                            .0
                            .escape_default()
                    )
                } else {
                    println!("#text {id}: {}", data.content.trim().escape_default())
                }
            }
        }

        NodeData::Comment { .. } => println!("<!-- COMMENT {id} -->"),

        NodeData::AnonymousBlock(_) => println!("{id} AnonymousBlock"),

        NodeData::Element(data) => {
            print!("<{} {id}", data.name.local);
            for attr in data.attrs.iter() {
                print!(" {}=\"{}\"", attr.name.local, attr.value);
            }
            if !node.children.is_empty() {
                println!(">");
            } else {
                println!("/>");
            }
        } // NodeData::Doctype {
          //     ref name,
          //     ref public_id,
          //     ref system_id,
          // } => println!("<!DOCTYPE {} \"{}\" \"{}\">", name, public_id, system_id),
          // NodeData::ProcessingInstruction { .. } => unreachable!(),
    }

    if !node.children.is_empty() {
        for child_id in node.children.iter() {
            walk_tree(indent + 2, node.with(*child_id));
        }

        if let NodeData::Element(data) = &node.data {
            println!("{}</{}>", " ".repeat(indent), data.name.local);
        }
    }
}

/// Whether `data` starts with gzip's magic bytes, as `usvg::Tree::from_data` checks
/// for SVGZ.
#[cfg(feature = "svg")]
pub(crate) fn is_gzip(data: &[u8]) -> bool {
    data.starts_with(&[0x1f, 0x8b])
}

/// Parse an SVG image.
///
/// An `<image href>` loads only from a `data:` URL. usvg's default resolver reads any
/// other href as a file path, outside the document's net provider, so an SVG from a
/// page could read local files, devices or (on Windows) UNC paths. A `data:` image
/// that is gzip-compressed is refused, like the SVG itself (see
/// [`SvgImageData::from_data`](crate::node::SvgImageData::from_data)), and so is a
/// raster too large to decode (see [`MAX_IMAGE_BYTES`](crate::net::MAX_IMAGE_BYTES)).
#[cfg(feature = "svg")]
pub(crate) fn parse_svg_image(source: &[u8]) -> Result<crate::node::SvgImageData, usvg::Error> {
    let default_data = usvg::ImageHrefResolver::default_data_resolver();
    let options = usvg::Options {
        fontdb: Arc::clone(&*FONT_DB),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(move |mime, data, options| {
                if is_gzip(&data) {
                    return None;
                }
                // A raster is decoded when the SVG is painted, in the format usvg
                // picked here from the MIME type or the data, so it is held to the
                // same size as the page's own images, read in that same format.
                // A raster goes on as `sanitize_image` leaves it.
                let mut kind = default_data(mime, data, options)?;
                let sanitized = match &kind {
                    usvg::ImageKind::JPEG(data) | usvg::ImageKind::PNG(data) => {
                        match crate::net::sanitize_image(data) {
                            std::borrow::Cow::Owned(data) => Some(Arc::new(data)),
                            std::borrow::Cow::Borrowed(_) => None,
                        }
                    }
                    _ => None,
                };
                if let Some(data) = sanitized {
                    kind = match kind {
                        usvg::ImageKind::JPEG(_) => usvg::ImageKind::JPEG(data),
                        _ => usvg::ImageKind::PNG(data),
                    };
                }
                let (format, data) = match &kind {
                    usvg::ImageKind::JPEG(data) => (image::ImageFormat::Jpeg, data),
                    usvg::ImageKind::PNG(data) => (image::ImageFormat::Png, data),
                    usvg::ImageKind::GIF(data) => (image::ImageFormat::Gif, data),
                    usvg::ImageKind::WEBP(data) => (image::ImageFormat::WebP, data),
                    usvg::ImageKind::SVG(_) => return Some(kind),
                };
                crate::net::check_image_size(data, Some(format), crate::net::MAX_IMAGE_BYTES)
                    .ok()?;
                Some(kind)
            }),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    crate::node::SvgImageData::from_data(source, &options)
}

pub trait ToColorColor {
    /// Converts a color into the `AlphaColor<Srgb>` type from the `color` crate
    fn as_color_color(&self) -> Color;
}
impl ToColorColor for AbsoluteColor {
    fn as_color_color(&self) -> Color {
        Color::new(
            *self
                .to_color_space(style::color::ColorSpace::Srgb)
                .raw_components(),
        )
    }
}

#[cfg(all(test, feature = "svg"))]
mod svg_tests {
    use super::parse_svg_image;

    // A 1x1 PNG.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn image_count(href: &str) -> usize {
        let src = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image href="{href}" width="10" height="10"/></svg>"#
        );
        fn count(group: &usvg::Group) -> usize {
            group
                .children()
                .iter()
                .map(|node| match node {
                    usvg::Node::Image(_) => 1,
                    usvg::Node::Group(group) => count(group),
                    _ => 0,
                })
                .sum()
        }
        count(parse_svg_image(src.as_bytes()).unwrap().tree.root())
    }

    #[test]
    fn image_href_with_a_file_path_loads_nothing() {
        // The file exists and is a valid PNG, so only the resolver keeps it out.
        let path = std::env::temp_dir().join(format!("blitz-svg-href-{}.png", std::process::id()));
        std::fs::write(&path, PNG).unwrap();
        let count = image_count(path.to_str().unwrap());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn gzip_compressed_svg_is_refused() {
        // A valid SVGZ of a 10x10 rect, which usvg would decompress and load.
        const SVGZ: &[u8] = &[
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xb3, 0x29, 0x2e, 0x4b,
            0x57, 0xa8, 0xc8, 0xcd, 0xc9, 0x2b, 0xb6, 0x55, 0xca, 0x28, 0x29, 0x29, 0xb0, 0xd2,
            0xd7, 0x2f, 0x2f, 0x2f, 0xd7, 0x2b, 0x37, 0xd6, 0xcb, 0x2f, 0x4a, 0xd7, 0x37, 0x32,
            0x30, 0x30, 0xd0, 0x07, 0xaa, 0x50, 0x52, 0x28, 0xcf, 0x4c, 0x29, 0xc9, 0xb0, 0x55,
            0x32, 0x34, 0x50, 0x52, 0xc8, 0x48, 0xcd, 0x4c, 0xcf, 0x28, 0x01, 0xb3, 0xed, 0x6c,
            0x8a, 0x52, 0x93, 0x4b, 0x70, 0x48, 0xea, 0xdb, 0xd9, 0x80, 0xf4, 0xda, 0x01, 0x00,
            0x27, 0x3c, 0x3f, 0xe9, 0x63, 0x00, 0x00, 0x00,
        ];
        assert!(parse_svg_image(SVGZ).is_err());
        // The same bytes, base64-encoded.
        let data = "H4sIAAAAAAAC/7MpLktXqMjNySu2VcooKSmw0tcvLy/XKzfWyy9K1zcyMDDQB6pQUijPTCnJsFUyNFBSyEjNTM8oAbPtbIpSk0twSOrb2YD02gEAJzw/6WMAAAA=";
        assert_eq!(image_count(&format!("data:image/svg+xml;base64,{data}")), 0);
    }

    #[test]
    fn image_href_with_a_raster_too_large_to_decode_loads_nothing() {
        // A PNG header, with no pixels, for 22000 by 22000 grayscale: 1.9 GB as RGBA.
        let data = "iVBORw0KGgoAAAANSUhEUgAAVfAAAFXwCAAAAABjOgE4AAAACElEQVR4nAMAAAAAAUgGidIAAAAASUVORK5CYII=";
        assert_eq!(image_count(&format!("data:image/png;base64,{data}")), 0);
    }

    #[test]
    fn image_href_with_a_jpeg_loads() {
        // Each raster is checked in the format usvg picked, so a JPEG reads as one.
        let mut jpeg = Vec::new();
        image::GrayImage::new(8, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let data: String = jpeg.iter().map(|b| format!("%{b:02X}")).collect();
        assert_eq!(image_count(&format!("data:image/jpeg,{data}")), 1);
    }

    /// The raster data of each image in an SVG holding one `<image>` of `href`.
    fn rasters(href: &str) -> Vec<std::sync::Arc<Vec<u8>>> {
        fn walk(group: &usvg::Group, found: &mut Vec<std::sync::Arc<Vec<u8>>>) {
            for node in group.children() {
                match node {
                    usvg::Node::Image(image) => match image.kind() {
                        usvg::ImageKind::JPEG(data)
                        | usvg::ImageKind::PNG(data)
                        | usvg::ImageKind::GIF(data)
                        | usvg::ImageKind::WEBP(data) => found.push(data.clone()),
                        usvg::ImageKind::SVG(_) => {}
                    },
                    usvg::Node::Group(group) => walk(group, found),
                    _ => {}
                }
            }
        }
        let src = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><image href="{href}" width="8" height="8"/></svg>"#
        );
        let mut found = Vec::new();
        walk(
            parse_svg_image(src.as_bytes()).unwrap().tree.root(),
            &mut found,
        );
        found
    }

    #[test]
    fn image_href_with_a_png_loads_without_its_icc_profile() {
        // A 2 by 2 grayscale PNG with an iCCP chunk.
        let data = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAAAAABX3VL4AAAAD2lDQ1BwAAB4nGNgoAwAAABAAAECndXUAAAADklEQVR4nGNoaGBoaAAABgYCASzBUNcAAAAASUVORK5CYII=";
        let found = rasters(&format!("data:image/png;base64,{data}"));
        assert_eq!(found.len(), 1);
        assert!(!found[0].windows(4).any(|w| w == b"iCCP"));
    }

    #[test]
    fn image_href_with_a_jpeg_loads_without_its_extended_xmp() {
        let mut jpeg = Vec::new();
        image::GrayImage::new(8, 8)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let mut segment = vec![0xff, 0xe1, 0x00, 0x2c];
        segment.extend_from_slice(b"http://ns.adobe.com/xmp/extension/\0");
        segment.extend_from_slice(&[0; 7]);
        jpeg.splice(2..2, segment);
        let data: String = jpeg.iter().map(|b| format!("%{b:02X}")).collect();
        let found = rasters(&format!("data:image/jpeg,{data}"));
        assert_eq!(found.len(), 1);
        assert!(!found[0].windows(5).any(|w| w == b"http:"));
    }

    #[test]
    fn image_href_with_a_data_url_loads() {
        // The same PNG as `PNG`.
        let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
        assert_eq!(image_count(&format!("data:image/png;base64,{data}")), 1);
    }

    #[test]
    fn svg_without_xmlns_parses() {
        let src = br#"<svg viewBox="0 0 24 24"><path d="M0 0h24v24H0z"/></svg>"#;
        let svg = parse_svg_image(src).unwrap();
        assert!(!svg.tree.root().children().is_empty());
    }

    #[test]
    fn missing_height_is_computed_from_width_and_viewbox_ratio() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="200"><rect width="100%" height="100%" fill="green"/></svg>"#;
        let svg = parse_svg_image(src).unwrap();
        assert_eq!(svg.intrinsic_width(), Some(200.0));
        assert_eq!(svg.intrinsic_height(), None);
        assert_eq!(svg.viewbox_aspect_ratio(), Some(1.0));
        assert_eq!(svg.tree.size().width(), 200.0);
        assert_eq!(svg.intrinsic_size(), (200.0, 200.0));
    }

    #[test]
    fn viewbox_only_has_no_intrinsic_dimensions() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 485 58"></svg>"#;
        let svg = parse_svg_image(src).unwrap();
        assert_eq!(svg.intrinsic_width(), None);
        assert_eq!(svg.intrinsic_height(), None);
        // The aspect ratio is still available from the viewBox.
        assert!((svg.aspect_ratio() - (485.0 / 58.0)).abs() < 1e-3);
    }

    #[test]
    fn absolute_dimensions_are_intrinsic() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="16" viewBox="0 0 48 32"></svg>"#;
        let svg = parse_svg_image(src).unwrap();
        assert_eq!(svg.intrinsic_width(), Some(24.0));
        assert_eq!(svg.intrinsic_height(), Some(16.0));
    }

    #[test]
    fn percentage_dimensions_are_not_intrinsic() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="50%" viewBox="0 0 200 100"></svg>"#;
        let svg = parse_svg_image(src).unwrap();
        assert_eq!(svg.intrinsic_width(), None);
        assert_eq!(svg.intrinsic_height(), None);
    }

    #[test]
    fn unit_lengths_are_intrinsic() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24px" height="1.5em" viewBox="0 0 48 32"></svg>"#;
        let svg = parse_svg_image(src).unwrap();
        assert!(svg.intrinsic_width().is_some());
        assert!(svg.intrinsic_height().is_some());
    }

    #[test]
    fn non_numeric_dimensions_are_not_intrinsic() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" width="auto" height="foo" viewBox="0 0 200 100"></svg>"#;
        let svg = parse_svg_image(src).unwrap();
        assert_eq!(svg.intrinsic_width(), None);
        assert_eq!(svg.intrinsic_height(), None);
    }
}

/// Creates an markup5ever::QualName.
/// Given a local name and an optional namespace
#[macro_export]
macro_rules! qual_name {
    ($local:tt $(, $ns:ident)?) => {
        $crate::QualName {
            prefix: None,
            ns: $crate::ns!($($ns)?),
            local: $crate::local_name!($local),
        }
    };
}
