use blitz_traits::node_id::NodeId;
use selectors::context::QuirksMode;
use std::sync::atomic::Ordering as Ao;
use std::{
    io::Cursor,
    sync::{Arc, atomic::AtomicUsize, mpsc::Sender},
};
use style::{
    font_face::{
        FontFaceSourceFormat, FontFaceSourceFormatKeyword, FontStyleRange, Source, SourceList,
    },
    media_queries::MediaList,
    servo_arc::Arc as ServoArc,
    shared_lock::SharedRwLock,
    shared_lock::{Locked, SharedRwLockReadGuard},
    stylesheets::{
        CssRule, DocumentStyleSheet, ImportRule, Origin, Stylesheet, StylesheetInDocument,
        StylesheetLoader as ServoStylesheetLoader, UrlExtraData,
        import_rule::{ImportLayer, ImportSheet, ImportSupportsCondition},
    },
    values::{CssUrl, SourceLocation},
};

use blitz_traits::net::{AbortSignal, Bytes, NetHandler, NetProvider, Request};
use blitz_traits::shell::ShellProvider;

use url::Url;

use crate::{document::DocumentEvent, util::ImageType};

pub(crate) fn stamped_request(url: Url, signal: Option<&AbortSignal>) -> Request {
    let mut req = Request::get(url);
    if let Some(sig) = signal {
        req = req.signal(sig.clone());
    }
    req
}

/// Carries `@font-face` descriptors from CSS parsing through to font
/// registration so `parley::Collection::register_fonts` can alias the bytes
/// under the `font-family` declared in CSS rather than whatever family name
/// the TTF's own `name` table reports.
///
/// All fields are `Option` because each descriptor is independently optional
/// at the CSS level. Missing fields fall back to the values parley reads
/// from the font's own metadata.
#[derive(Clone, Debug, Default)]
pub struct FontFaceOverrides {
    /// `font-family` descriptor (the alias the rest of the stylesheet uses).
    pub family_name: Option<String>,
    /// `font-weight` descriptor as a single CSS weight (100–900). Stylo
    /// parses this as a range; we record the lower bound, which equals the
    /// upper bound in the common single-value case.
    pub weight: Option<f32>,
    /// `font-style` descriptor mapped to fontique's `FontStyle`.
    pub style: Option<parley::fontique::FontStyle>,
}

#[derive(Clone, Debug)]
pub enum Resource {
    Image(ImageType, u32, u32, Arc<Vec<u8>>),
    #[cfg(feature = "svg")]
    Svg(ImageType, crate::node::SvgImageData),
    Css(DocumentStyleSheet),
    /// Stylesheet loaded for an `@import` rule, to be attached to the rule on the document thread
    ImportedCss(ServoArc<Locked<ImportRule>>, ServoArc<Stylesheet>),
    Font(Bytes, FontFaceOverrides),
    /// HTML fetched for an `<iframe>` element's `src`
    DocumentSrc(String),
    None,
}

pub(crate) struct ResourceHandler<T: Send + Sync + 'static> {
    doc_id: usize,
    request_id: usize,
    node_id: Option<NodeId>,
    tx: Sender<DocumentEvent>,
    shell_provider: Arc<dyn ShellProvider>,
    data: T,
}

impl<T: Send + Sync + 'static> ResourceHandler<T> {
    pub(crate) fn new(
        tx: Sender<DocumentEvent>,
        doc_id: usize,
        node_id: Option<NodeId>,
        shell_provider: Arc<dyn ShellProvider>,
        data: T,
    ) -> Self {
        static REQUEST_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
        Self {
            request_id: REQUEST_ID_COUNTER.fetch_add(1, Ao::Relaxed),
            doc_id,
            node_id,
            tx,
            shell_provider,
            data,
        }
    }

    pub(crate) fn boxed(
        tx: Sender<DocumentEvent>,
        doc_id: usize,
        node_id: Option<NodeId>,
        shell_provider: Arc<dyn ShellProvider>,
        data: T,
    ) -> Box<dyn NetHandler>
    where
        ResourceHandler<T>: NetHandler,
    {
        Box::new(Self::new(tx, doc_id, node_id, shell_provider, data)) as _
    }

    pub(crate) fn request_id(&self) -> usize {
        self.request_id
    }

    fn respond(&self, resolved_url: String, result: Result<Resource, String>) {
        let response = ResourceLoadResponse {
            request_id: self.request_id,
            node_id: self.node_id,
            resolved_url: Some(resolved_url),
            result,
        };
        let _ = self.tx.send(DocumentEvent::ResourceLoad(response));
        self.shell_provider.request_redraw();
    }
}

#[allow(unused)]
pub struct ResourceLoadResponse {
    pub request_id: usize,
    pub node_id: Option<NodeId>,
    pub resolved_url: Option<String>,
    pub result: Result<Resource, String>,
}

pub struct StylesheetHandler {
    pub source_url: Url,
    pub guard: SharedRwLock,
    pub net_provider: Arc<dyn NetProvider>,
    pub abort_signal: Option<AbortSignal>,
}

impl NetHandler for ResourceHandler<StylesheetHandler> {
    fn error(self: Box<Self>, url: String, error: String) {
        self.respond(url, Err(error));
    }

    fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
        // NOTE(Nico): I don't *think* external stylesheets should have HTML entities escaped
        // let escaped_css = html_escape::decode_html_entities(css);

        let sheet = Stylesheet::from_bytes(
            &bytes,
            self.data.source_url.clone().into(),
            None,
            None,
            Origin::Author,
            ServoArc::new(self.data.guard.wrap(MediaList::empty())),
            self.data.guard.clone(),
            Some(&StylesheetLoader {
                tx: self.tx.clone(),
                doc_id: self.doc_id,
                net_provider: self.data.net_provider.clone(),
                shell_provider: self.shell_provider.clone(),
                abort_signal: self.data.abort_signal.clone(),
                import_depth: 0,
            }),
            None, // error_reporter
            QuirksMode::NoQuirks,
        );

        self.respond(
            resolved_url,
            Ok(Resource::Css(DocumentStyleSheet(ServoArc::new(sheet)))),
        );
    }
}

/// Maximum depth of nested `@import` rules. Imports nested deeper than this
/// are refused, preventing unbounded recursion (e.g. a stylesheet whose
/// resolved `@import` URL yields another stylesheet importing in turn, with
/// the URL growing geometrically at each level).
const MAX_IMPORT_DEPTH: usize = 16;

#[derive(Clone)]
pub(crate) struct StylesheetLoader {
    pub(crate) tx: Sender<DocumentEvent>,
    pub(crate) doc_id: usize,
    pub(crate) net_provider: Arc<dyn NetProvider>,
    pub(crate) shell_provider: Arc<dyn ShellProvider>,
    pub(crate) abort_signal: Option<AbortSignal>,
    /// Depth of `@import` nesting for the stylesheet this loader is parsing
    /// (0 for a top-level stylesheet)
    pub(crate) import_depth: usize,
}
impl ServoStylesheetLoader for StylesheetLoader {
    fn request_stylesheet(
        &self,
        url: CssUrl,
        location: SourceLocation,
        lock: &SharedRwLock,
        media: ServoArc<Locked<MediaList>>,
        supports: Option<ImportSupportsCondition>,
        layer: ImportLayer,
    ) -> ServoArc<Locked<ImportRule>> {
        if self.import_depth >= MAX_IMPORT_DEPTH || !supports.as_ref().is_none_or(|s| s.enabled) {
            return ServoArc::new(lock.wrap(ImportRule {
                url,
                stylesheet: ImportSheet::new_refused(),
                supports,
                layer,
                source_location: location,
            }));
        }

        let import = ImportRule {
            url,
            stylesheet: ImportSheet::new_pending(),
            supports,
            layer,
            source_location: location,
        };

        let url = import.url.url().unwrap().clone();
        let import = ServoArc::new(lock.wrap(import));
        self.net_provider.fetch(
            self.doc_id,
            stamped_request(url.as_ref().clone(), self.abort_signal.as_ref()),
            ResourceHandler::boxed(
                self.tx.clone(),
                self.doc_id,
                None, // node_id
                self.shell_provider.clone(),
                NestedStylesheetHandler {
                    url: url.clone(),
                    loader: StylesheetLoader {
                        import_depth: self.import_depth + 1,
                        ..self.clone()
                    },
                    lock: lock.clone(),
                    media,
                    import_rule: import.clone(),
                    net_provider: self.net_provider.clone(),
                },
            ),
        );

        import
    }
}

struct NestedStylesheetHandler {
    loader: StylesheetLoader,
    lock: SharedRwLock,
    url: ServoArc<Url>,
    media: ServoArc<Locked<MediaList>>,
    import_rule: ServoArc<Locked<ImportRule>>,
    net_provider: Arc<dyn NetProvider>,
}

impl NetHandler for ResourceHandler<NestedStylesheetHandler> {
    fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
        // NOTE(Nico): I don't *think* external stylesheets should have HTML entities escaped
        // let escaped_css = html_escape::decode_html_entities(css);

        let sheet = ServoArc::new(Stylesheet::from_bytes(
            &bytes,
            UrlExtraData(self.data.url.clone()),
            None,
            None,
            Origin::Author,
            self.data.media.clone(),
            self.data.lock.clone(),
            Some(&self.data.loader),
            None, // error_reporter
            QuirksMode::NoQuirks,
        ));

        // Fetch @font-face fonts
        fetch_font_face(
            self.tx.clone(),
            self.doc_id,
            self.node_id,
            &sheet,
            &self.data.net_provider,
            &self.shell_provider,
            &self.data.lock.read(),
            self.data.loader.abort_signal.as_ref(),
        );

        let import_rule = self.data.import_rule.clone();
        self.respond(resolved_url, Ok(Resource::ImportedCss(import_rule, sheet)))
    }
}

struct FontFaceHandler {
    format: FontFaceSourceFormatKeyword,
    overrides: FontFaceOverrides,
}
impl NetHandler for ResourceHandler<FontFaceHandler> {
    fn bytes(mut self: Box<Self>, resolved_url: String, bytes: Bytes) {
        let result = self.data.parse(bytes);
        self.respond(resolved_url, result)
    }
}
impl FontFaceHandler {
    fn parse(&mut self, bytes: Bytes) -> Result<Resource, String> {
        if self.format == FontFaceSourceFormatKeyword::None && bytes.len() >= 4 {
            self.format = match &bytes.as_ref()[0..4] {
                // WOFF (v1) files begin with 0x774F4646 ('wOFF' in ascii)
                // See: <https://w3c.github.io/woff/woff1/spec/Overview.html#WOFFHeader>
                #[cfg(feature = "woff")]
                b"wOFF" => FontFaceSourceFormatKeyword::Woff,
                // WOFF2 files begin with 0x774F4632 ('wOF2' in ascii)
                // See: <https://w3c.github.io/woff/woff2/#woff20Header>
                #[cfg(feature = "woff")]
                b"wOF2" => FontFaceSourceFormatKeyword::Woff2,
                // Opentype fonts with CFF data begin with 0x4F54544F ('OTTO' in ascii)
                // See: <https://learn.microsoft.com/en-us/typography/opentype/spec/otff#organization-of-an-opentype-font>
                b"OTTO" => FontFaceSourceFormatKeyword::Opentype,
                // Opentype fonts truetype outlines begin with 0x00010000
                // See: <https://learn.microsoft.com/en-us/typography/opentype/spec/otff#organization-of-an-opentype-font>
                &[0x00, 0x01, 0x00, 0x00] => FontFaceSourceFormatKeyword::Truetype,
                // Truetype fonts begin with 0x74727565 ('true' in ascii)
                // See: <https://developer.apple.com/fonts/TrueType-Reference-Manual/RM06/Chap6.html#ScalerTypeNote>
                b"true" => FontFaceSourceFormatKeyword::Truetype,
                _ => FontFaceSourceFormatKeyword::None,
            }
        }

        // Satisfy rustc's mutability linting with woff feature both enabled/disabled
        #[cfg(feature = "woff")]
        let mut bytes = bytes;

        match self.format {
            #[cfg(feature = "woff")]
            FontFaceSourceFormatKeyword::Woff => {
                #[cfg(feature = "tracing")]
                tracing::info!("Decompressing woff1 font");

                // Use wuff crate to decompress font
                let decompressed = wuff::decompress_woff1(&bytes).ok();

                if let Some(decompressed) = decompressed {
                    bytes = Bytes::from(decompressed);
                } else {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("Failed to decompress woff1 font");
                }
            }
            #[cfg(feature = "woff")]
            FontFaceSourceFormatKeyword::Woff2 => {
                #[cfg(feature = "tracing")]
                tracing::info!("Decompressing woff2 font");

                // Use wuff crate to decompress font
                let decompressed = wuff::decompress_woff2(&bytes).ok();

                if let Some(decompressed) = decompressed {
                    bytes = Bytes::from(decompressed);
                } else {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("Failed to decompress woff2 font");
                }
            }
            FontFaceSourceFormatKeyword::None => {
                // Should this be an error?
                return Ok(Resource::None);
            }
            _ => {}
        }

        Ok(Resource::Font(bytes, std::mem::take(&mut self.overrides)))
    }
}

/// Fetch the fonts of all `@font-face` rules in `sheet`
#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_font_face(
    tx: Sender<DocumentEvent>,
    doc_id: usize,
    node_id: Option<NodeId>,
    sheet: &Stylesheet,
    network_provider: &Arc<dyn NetProvider>,
    shell_provider: &Arc<dyn ShellProvider>,
    read_guard: &SharedRwLockReadGuard,
    abort_signal: Option<&AbortSignal>,
) {
    fetch_font_face_rules(
        sheet.contents(read_guard).rules(read_guard).iter(),
        tx,
        doc_id,
        node_id,
        network_provider,
        shell_provider,
        read_guard,
        abort_signal,
    )
}

/// Fetch the fonts of the `@font-face` rules among `rules`
#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_font_face_rules<'a>(
    rules: impl Iterator<Item = &'a CssRule>,
    tx: Sender<DocumentEvent>,
    doc_id: usize,
    node_id: Option<NodeId>,
    network_provider: &Arc<dyn NetProvider>,
    shell_provider: &Arc<dyn ShellProvider>,
    read_guard: &SharedRwLockReadGuard,
    abort_signal: Option<&AbortSignal>,
) {
    rules
        .filter_map(|rule| match rule {
            CssRule::FontFace(font_face) => {
                let descriptor = &font_face.read_with(read_guard).descriptors;
                let family = descriptor.font_family.as_ref()?;
                let src = descriptor.src.as_ref()?;
                // Capture the @font-face descriptors so parley can register
                // the font under the CSS-declared family name (and weight /
                // style) rather than whatever metadata the TTF reports.
                let overrides = FontFaceOverrides {
                    family_name: Some(family.name.to_string()),
                    weight: descriptor
                        .font_weight
                        .as_ref()
                        .and_then(|range| range.0.compute().map(|w| w.value())),
                    style: descriptor.font_style.as_ref().map(stylo_to_fontique_style),
                };
                Some((src, overrides))
            }
            _ => None,
        })
        .for_each(|(source_list, overrides)| {
            if let Some((url, format)) = preferred_font_source(source_list) {
                network_provider.fetch(
                    doc_id,
                    stamped_request(url, abort_signal),
                    ResourceHandler::boxed(
                        tx.clone(),
                        doc_id,
                        node_id,
                        shell_provider.clone(),
                        FontFaceHandler { format, overrides },
                    ),
                );
            }
        })
}

/// The first source in an `@font-face` `src` list that is in a format we
/// support, with its resolved url and format.
fn preferred_font_source(source_list: &SourceList) -> Option<(Url, FontFaceSourceFormatKeyword)> {
    source_list.0.iter().find_map(|source| {
        let url_source = match source {
            Source::Url(url_source) => url_source,
            // TODO: support local fonts in @font-face
            Source::Local(_) => return None,
        };

        // A relative url with no base url to resolve against
        // yields None; skip the source instead of panicking
        let Some(url) = url_source.url.url() else {
            #[cfg(feature = "tracing")]
            tracing::warn!("Skipping @font-face source with unresolvable url");
            return None;
        };

        let format = match &url_source.format_hint {
            Some(FontFaceSourceFormat::Keyword(keyword)) => *keyword,
            Some(FontFaceSourceFormat::String(string)) => font_format_from_string(string),
            None => font_format_from_extension(url),
        };

        let supported = match format {
            FontFaceSourceFormatKeyword::None
            | FontFaceSourceFormatKeyword::Collection
            | FontFaceSourceFormatKeyword::Opentype
            | FontFaceSourceFormatKeyword::Truetype => true,
            FontFaceSourceFormatKeyword::Woff | FontFaceSourceFormatKeyword::Woff2 => {
                cfg!(feature = "woff")
            }
            FontFaceSourceFormatKeyword::EmbeddedOpentype
            | FontFaceSourceFormatKeyword::Svg
            | FontFaceSourceFormatKeyword::Unknown => false,
        };
        if !supported {
            #[cfg(feature = "tracing")]
            tracing::warn!("Skipping unsupported font of type {:?}", format);
            return None;
        }

        Some((url.as_ref().clone(), format))
    })
}

/// Map a `format("...")` string hint to a format. Besides the keywords, this
/// accepts the legacy `-variations` strings and the `ttf`/`otf` extensions.
fn font_format_from_string(format: &str) -> FontFaceSourceFormatKeyword {
    const FORMATS: &[(&str, FontFaceSourceFormatKeyword)] = &[
        ("collection", FontFaceSourceFormatKeyword::Collection),
        (
            "embedded-opentype",
            FontFaceSourceFormatKeyword::EmbeddedOpentype,
        ),
        ("opentype", FontFaceSourceFormatKeyword::Opentype),
        ("opentype-variations", FontFaceSourceFormatKeyword::Opentype),
        ("otf", FontFaceSourceFormatKeyword::Opentype),
        ("svg", FontFaceSourceFormatKeyword::Svg),
        ("truetype", FontFaceSourceFormatKeyword::Truetype),
        ("truetype-variations", FontFaceSourceFormatKeyword::Truetype),
        ("ttf", FontFaceSourceFormatKeyword::Truetype),
        ("woff", FontFaceSourceFormatKeyword::Woff),
        ("woff-variations", FontFaceSourceFormatKeyword::Woff),
        ("woff2", FontFaceSourceFormatKeyword::Woff2),
        ("woff2-variations", FontFaceSourceFormatKeyword::Woff2),
    ];
    FORMATS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(format))
        .map_or(FontFaceSourceFormatKeyword::Unknown, |(_, format)| *format)
}

/// Guess a font's format from the file extension of its url's path. Urls
/// with no known extension, such as `data:` urls, get
/// `FontFaceSourceFormatKeyword::None` and are sniffed from their bytes once
/// fetched.
fn font_format_from_extension(url: &Url) -> FontFaceSourceFormatKeyword {
    const EXTENSIONS: &[(&str, FontFaceSourceFormatKeyword)] = &[
        ("eot", FontFaceSourceFormatKeyword::EmbeddedOpentype),
        ("otf", FontFaceSourceFormatKeyword::Opentype),
        ("svg", FontFaceSourceFormatKeyword::Svg),
        ("ttc", FontFaceSourceFormatKeyword::Collection),
        ("ttf", FontFaceSourceFormatKeyword::Truetype),
        ("woff", FontFaceSourceFormatKeyword::Woff),
        ("woff2", FontFaceSourceFormatKeyword::Woff2),
    ];
    if url.cannot_be_a_base() {
        return FontFaceSourceFormatKeyword::None;
    }
    let file_name = url.path().rsplit('/').next().unwrap_or_default();
    let Some((_, extension)) = file_name.rsplit_once('.') else {
        return FontFaceSourceFormatKeyword::None;
    };
    EXTENSIONS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(extension))
        .map_or(FontFaceSourceFormatKeyword::None, |(_, format)| *format)
}

/// Translate stylo's `@font-face` `font-style` descriptor into the fontique
/// `FontStyle` enum used by parley. Stylo encodes Italic and Oblique-with-
/// angle distinctly; CSS's bare `normal` is parsed as `Oblique(0deg, 0deg)`
/// by stylo (see the `FontStyle::parse` impl in stylo's `font_face.rs`), so
/// that pattern is treated as `Normal` here.
fn stylo_to_fontique_style(style: &FontStyleRange) -> parley::fontique::FontStyle {
    use parley::fontique::FontStyle as Fq;
    match style {
        FontStyleRange::Italic => Fq::Italic,
        FontStyleRange::Oblique(min, max) => {
            let angle = min.degrees();
            // Stylo emits `Oblique(0deg, 0deg)` for the literal CSS `normal`
            // keyword. Map that back to `Normal` so parley's font matching
            // doesn't misclassify upright fonts.
            if angle.is_none_or(|a| a == 0.0) && max.degrees().is_none_or(|a| a == 0.0) {
                Fq::Normal
            } else {
                Fq::Oblique(angle)
            }
        }
    }
}

/// Handles HTML fetched for an `<iframe>` element's `src`
pub(crate) struct DocumentSrcHandler;

impl NetHandler for ResourceHandler<DocumentSrcHandler> {
    fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
        let html = String::from_utf8_lossy(&bytes).into_owned();
        self.respond(resolved_url, Ok(Resource::DocumentSrc(html)));
    }
}

pub struct ImageHandler {
    kind: ImageType,
}
impl ImageHandler {
    pub fn new(kind: ImageType) -> Self {
        Self { kind }
    }
}

impl NetHandler for ResourceHandler<ImageHandler> {
    fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
        let result = self.data.parse(bytes);
        self.respond(resolved_url, result)
    }
}

/// The largest image decoded, in bytes, both as 8-bit RGBA and as the decoder's own
/// buffer (a 16-bit image takes twice the RGBA). As RGBA, 128 MiB is 5792 pixels
/// square, or 8000 by 4194. A page's images are untrusted, and a small file can
/// declare a huge one, so a larger image is refused from its header, before it is
/// decoded.
pub(crate) const MAX_IMAGE_BYTES: u64 = 128 * 1024 * 1024;

fn reader(bytes: &[u8]) -> image::ImageReader<Cursor<&[u8]>> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .expect("IO errors impossible with Cursor")
}

/// The most extended-XMP segments a JPEG may have. zune-jpeg sorts and walks all
/// it has seen at every marker after the first, so a header read takes time in the
/// square of their number: 80,000 of them, 6.4 MB, took 15 s.
const MAX_JPEG_EXTENDED_XMP: usize = 64;

/// Refuses the image in `bytes`, read in `format` or the one its bytes show, from
/// its header alone, if decoding it would take more than `max` bytes, as 8-bit RGBA
/// or in the decoder's own format. Any header it can't read is refused too, and so is
/// a JPEG with more than [`MAX_JPEG_EXTENDED_XMP`] extended-XMP segments.
pub(crate) fn check_image_size(
    bytes: &[u8],
    format: Option<image::ImageFormat>,
    max: u64,
) -> image::ImageResult<()> {
    use image::ImageDecoder as _;
    let refused = || {
        image::ImageError::Limits(image::error::LimitError::from_kind(
            image::error::LimitErrorKind::DimensionError,
        ))
    };
    let mut reader = match format {
        Some(format) => image::ImageReader::with_format(Cursor::new(bytes), format),
        None => reader(bytes),
    };
    if reader.format() == Some(image::ImageFormat::Jpeg) {
        // Counted anywhere in the file, so that no marker zune-jpeg finds is missed.
        const PREFIX: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
        let segments = bytes.windows(PREFIX.len()).filter(|w| *w == PREFIX);
        if segments.take(MAX_JPEG_EXTENDED_XMP + 1).count() > MAX_JPEG_EXTENDED_XMP {
            return Err(refused());
        }
    }
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(max);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    let rgba = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4);
    if rgba > max || decoder.total_bytes() > max {
        return Err(refused());
    }
    Ok(())
}

/// Decodes `bytes` as an image, without a PNG's ICC profiles (see
/// [`strip_png_iccp`]), unless [`check_image_size`] refuses it. The decode
/// may allocate twice `max`: a GIF frame smaller or larger than its image takes a
/// buffer of its own beside the image, which the header doesn't show.
fn decode_image(bytes: &[u8], max: u64) -> image::ImageResult<image::DynamicImage> {
    let bytes = &strip_png_iccp(bytes);
    check_image_size(bytes, None, max)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(max.saturating_mul(2));
    let mut reader = reader(bytes);
    reader.limits(limits);
    reader.decode()
}

/// `bytes` without the ICC profiles (iCCP chunks) of a PNG, which is borrowed
/// unchanged if it is no PNG or has none. The png crate inflates a profile while it
/// reads the header, as far as its memory limit, and a small file can hold one of
/// hundreds of megabytes; nothing here uses it. Chunks after the first IDAT, and
/// bytes past one that runs off the end, are kept as they are.
pub(crate) fn strip_png_iccp(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(SIGNATURE) {
        return bytes.into();
    }
    let mut kept = SIGNATURE.to_vec();
    let mut stripped = false;
    let mut pos = SIGNATURE.len();
    while let Some(head) = bytes.get(pos..pos + 8) {
        let len = u32::from_be_bytes(head[..4].try_into().unwrap()) as usize;
        let end = pos.checked_add(12).and_then(|end| end.checked_add(len));
        let Some(end) = end.filter(|&end| end <= bytes.len()) else {
            break;
        };
        match &head[4..] {
            b"IDAT" => break,
            b"iCCP" => stripped = true,
            _ => kept.extend_from_slice(&bytes[pos..end]),
        }
        pos = end;
    }
    if !stripped {
        return bytes.into();
    }
    kept.extend_from_slice(&bytes[pos..]);
    kept.into()
}

impl ImageHandler {
    fn parse(&self, bytes: Bytes) -> Result<Resource, String> {
        let image_err = match decode_image(&bytes, MAX_IMAGE_BYTES) {
            Ok(image) => {
                let width = image.width();
                let height = image.height();
                let raw_rgba8_data = image.into_rgba8().into_raw();
                return Ok(Resource::Image(
                    self.kind,
                    width,
                    height,
                    Arc::new(raw_rgba8_data),
                ));
            }
            Err(e) => e.to_string(),
        };

        #[cfg(feature = "svg")]
        let svg_err = {
            use crate::util::parse_svg_image;
            match parse_svg_image(&bytes) {
                Ok(svg) => return Ok(Resource::Svg(self.kind, svg)),
                Err(e) => e.to_string(),
            }
        };
        #[cfg(not(feature = "svg"))]
        let svg_err = "svg feature disabled";

        Err(format!(
            "Could not parse image ({} bytes): image-crate error: {image_err}; svg fallback error: {svg_err}",
            bytes.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use parley::fontique::FontStyle as Fq;
    use style::stylesheets::AllowImportRules;
    use style::values::specified::Angle;

    /// Whether `result` is [`check_image_size`]'s refusal.
    fn refused<T>(result: &image::ImageResult<T>) -> bool {
        matches!(result, Err(image::ImageError::Limits(e))
            if e.kind() == image::error::LimitErrorKind::DimensionError)
    }

    // PNG headers with no pixel data: the signature, IHDR, an empty IDAT and IEND.
    /// 22000 by 22000 grayscale: 1.9 GB as RGBA, from a file of 65 bytes.
    const HUGE_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x55, 0xf0, 0x00, 0x00, 0x55, 0xf0, 0x08, 0x00, 0x00, 0x00, 0x00, 0x63,
        0x3a, 0x01, 0x38, 0x00, 0x00, 0x00, 0x08, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x03, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x48, 0x06, 0x89, 0xd2, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
        0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    /// 4000 by 3000 RGB, a large photo: 48 MB as RGBA.
    const PHOTO_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x0f, 0xa0, 0x00, 0x00, 0x0b, 0xb8, 0x08, 0x02, 0x00, 0x00, 0x00, 0x7a,
        0xd7, 0xd0, 0xb0, 0x00, 0x00, 0x00, 0x08, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x03, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x48, 0x06, 0x89, 0xd2, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
        0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    /// 5000 by 5000 16-bit RGBA: 100 MB as 8-bit RGBA, but 200 MB to decode.
    const DEEP_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x13, 0x88, 0x00, 0x00, 0x13, 0x88, 0x10, 0x06, 0x00, 0x00, 0x00, 0x0d,
        0x08, 0x5b, 0x88, 0x00, 0x00, 0x00, 0x08, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x03, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x48, 0x06, 0x89, 0xd2, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
        0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn an_image_too_large_to_decode_is_refused_from_its_header() {
        assert!(refused(&decode_image(HUGE_PNG, MAX_IMAGE_BYTES)));
        assert!(refused(&decode_image(DEEP_PNG, MAX_IMAGE_BYTES)));
        // The photo passes the check, then fails here only for its missing pixels.
        assert!(check_image_size(PHOTO_PNG, None, MAX_IMAGE_BYTES).is_ok());
    }

    #[test]
    fn a_gif_frame_larger_than_its_image_is_refused() {
        // A 1 by 1 GIF, two black colours, whose one frame is 10000 by 10000: the frame takes a 400 MB
        // buffer of its own, which the header doesn't show.
        let gif = [
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x10, 0x27, 0x10, 0x27,
            0x00, 0x02, 0x02, 0x4c, 0x01, 0x00, 0x3b,
        ];
        assert!(check_image_size(&gif, None, MAX_IMAGE_BYTES).is_ok());
        let error = decode_image(&gif, MAX_IMAGE_BYTES).unwrap_err();
        assert!(matches!(error, image::ImageError::Limits(_)), "{error}");
    }

    #[test]
    fn a_png_loses_its_icc_profile_before_decoding() {
        // A 2 by 2 grayscale PNG with a 27-byte iCCP chunk before its IDAT.
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0x00,
            0x00, 0x57, 0xdd, 0x52, 0xf8, 0x00, 0x00, 0x00, 0x0f, 0x69, 0x43, 0x43, 0x50, 0x70,
            0x00, 0x00, 0x78, 0x9c, 0x63, 0x60, 0xa0, 0x0c, 0x00, 0x00, 0x00, 0x40, 0x00, 0x01,
            0x02, 0x9d, 0xd5, 0xd4, 0x00, 0x00, 0x00, 0x0e, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c,
            0x63, 0x68, 0x68, 0x60, 0x68, 0x68, 0x00, 0x00, 0x06, 0x06, 0x02, 0x01, 0x2c, 0xc1,
            0x50, 0xd7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let stripped = strip_png_iccp(&png);
        assert_eq!(stripped.len(), png.len() - 27);
        assert!(!stripped.windows(4).any(|w| w == b"iCCP"));
        assert_eq!(
            decode_image(&stripped, MAX_IMAGE_BYTES)
                .unwrap()
                .into_luma8()
                .into_raw(),
            [0x80; 4]
        );
        // Without a profile, or as no PNG, the bytes pass unchanged.
        assert!(matches!(
            strip_png_iccp(&stripped),
            std::borrow::Cow::Borrowed(_)
        ));
        assert!(matches!(
            strip_png_iccp(b"GIF89a"),
            std::borrow::Cow::Borrowed(_)
        ));
        // A chunk running off the end stops the walk, and the rest is kept.
        let cut = &png[..png.len() - 20];
        assert_eq!(strip_png_iccp(cut).len(), cut.len() - 27);
    }

    /// A JPEG of SOI, `segments` extended-XMP segments of one never-complete XMP, and
    /// EOI.
    fn jpeg_with_extended_xmp(segments: u32) -> Vec<u8> {
        let mut jpeg = vec![0xff, 0xd8];
        for offset in 0..segments {
            let mut payload = b"http://ns.adobe.com/xmp/extension/\0".to_vec();
            payload.extend_from_slice(&[b'0'; 32]); // The XMP's GUID.
            payload.extend_from_slice(&u32::MAX.to_be_bytes()); // Its full length.
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.push(0);
            jpeg.extend_from_slice(&[0xff, 0xe1]);
            jpeg.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
            jpeg.extend_from_slice(&payload);
        }
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        jpeg
    }

    #[test]
    fn a_jpeg_with_too_many_extended_xmp_segments_is_refused() {
        let at_limit = jpeg_with_extended_xmp(MAX_JPEG_EXTENDED_XMP as u32);
        assert!(!refused(&check_image_size(
            &at_limit,
            None,
            MAX_IMAGE_BYTES
        )));
        let over = jpeg_with_extended_xmp(MAX_JPEG_EXTENDED_XMP as u32 + 1);
        assert!(refused(&check_image_size(&over, None, MAX_IMAGE_BYTES)));
    }

    #[test]
    fn an_image_at_the_limit_decodes() {
        // A 2 by 2 grayscale PNG: 16 bytes as RGBA.
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0x00,
            0x00, 0x57, 0xdd, 0x52, 0xf8, 0x00, 0x00, 0x00, 0x0e, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x68, 0x68, 0x60, 0x68, 0x68, 0x00, 0x00, 0x06, 0x06, 0x02, 0x01, 0x2c,
            0xc1, 0x50, 0xd7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60,
            0x82,
        ];
        assert!(decode_image(&png, 16).is_ok());
        assert!(refused(&decode_image(&png, 15)));
    }

    fn oblique(min_deg: f32, max_deg: f32) -> FontStyleRange {
        FontStyleRange::Oblique(Angle::from_degrees(min_deg), Angle::from_degrees(max_deg))
    }

    /// The source picked from the `src` list of each `@font-face` rule in `css`
    fn preferred_sources(css: &str) -> Vec<Option<(String, FontFaceSourceFormatKeyword)>> {
        let lock = SharedRwLock::new();
        let base_url = Url::parse("https://example.com/css/style.css").unwrap();
        let sheet = Stylesheet::from_str(
            css,
            UrlExtraData(ServoArc::new(base_url)),
            Origin::Author,
            ServoArc::new(lock.wrap(MediaList::empty())),
            lock.clone(),
            None,
            None,
            QuirksMode::NoQuirks,
            AllowImportRules::No,
        );
        let guard = lock.read();
        sheet
            .contents(&guard)
            .rules(&guard)
            .iter()
            .filter_map(|rule| match rule {
                CssRule::FontFace(font_face) => font_face.read_with(&guard).descriptors.src.clone(),
                _ => None,
            })
            .map(|src| preferred_font_source(&src).map(|(url, format)| (url.to_string(), format)))
            .collect()
    }

    #[test]
    fn bootstrap_glyphicons_skip_eot_source() {
        let sources = preferred_sources(
            "@font-face {
                font-family: 'Glyphicons Halflings';
                src: url('../fonts/glyphicons-halflings-regular.eot');
                src: url('../fonts/glyphicons-halflings-regular.eot?#iefix') format('embedded-opentype'),
                    url('../fonts/glyphicons-halflings-regular.woff2') format('woff2'),
                    url('../fonts/glyphicons-halflings-regular.woff') format('woff'),
                    url('../fonts/glyphicons-halflings-regular.ttf') format('truetype'),
                    url('../fonts/glyphicons-halflings-regular.svg#glyphicons_halflingsregular') format('svg');
            }",
        );
        let expected = if cfg!(feature = "woff") {
            (
                "https://example.com/fonts/glyphicons-halflings-regular.woff2",
                FontFaceSourceFormatKeyword::Woff2,
            )
        } else {
            (
                "https://example.com/fonts/glyphicons-halflings-regular.ttf",
                FontFaceSourceFormatKeyword::Truetype,
            )
        };
        assert_eq!(sources, [Some((expected.0.to_string(), expected.1))]);
    }

    #[test]
    fn format_strings_map_to_formats() {
        use FontFaceSourceFormatKeyword as Kw;
        for (string, format) in [
            ("collection", Kw::Collection),
            ("embedded-opentype", Kw::EmbeddedOpentype),
            ("opentype", Kw::Opentype),
            ("OpenType", Kw::Opentype),
            ("opentype-variations", Kw::Opentype),
            ("otf", Kw::Opentype),
            ("svg", Kw::Svg),
            ("truetype", Kw::Truetype),
            ("truetype-variations", Kw::Truetype),
            ("ttf", Kw::Truetype),
            ("woff", Kw::Woff),
            ("woff-variations", Kw::Woff),
            ("woff2", Kw::Woff2),
            ("woff2-variations", Kw::Woff2),
            ("x-unknown", Kw::Unknown),
        ] {
            assert_eq!(font_format_from_string(string), format, "{string}");
        }
    }

    #[test]
    fn extension_ignores_query_and_fragment() {
        use FontFaceSourceFormatKeyword as Kw;
        for (url, format) in [
            ("https://example.com/a.eot?#iefix", Kw::EmbeddedOpentype),
            ("https://example.com/a.svg#font", Kw::Svg),
            ("https://example.com/a.TTF?v=4.7.0", Kw::Truetype),
            ("https://example.com/a.otf", Kw::Opentype),
            ("https://example.com/a.woff2?v=1#x", Kw::Woff2),
            ("https://example.com/fonts.v2/font", Kw::None),
            ("https://example.com/font?name=a.ttf", Kw::None),
            ("data:font/ttf;base64,AAEAAA.ttf", Kw::None),
        ] {
            let url = Url::parse(url).unwrap();
            assert_eq!(font_format_from_extension(&url), format, "{url}");
        }
    }

    #[test]
    fn unsupported_sources_are_skipped() {
        use FontFaceSourceFormatKeyword as Kw;
        let sources = preferred_sources(
            "@font-face { font-family: a; src: url(a.ttf) format('x-unknown'), url(a.otf); }
            @font-face { font-family: b; src: url(b.svg#b), url(b.eot), url(b.ttf?v=1#x); }
            @font-face { font-family: c; src: url(c.eot) format(embedded-opentype); }
            @font-face { font-family: d; src: url(d.eot) format(truetype); }",
        );
        assert_eq!(
            sources,
            [
                Some(("https://example.com/css/a.otf".to_string(), Kw::Opentype)),
                Some((
                    "https://example.com/css/b.ttf?v=1#x".to_string(),
                    Kw::Truetype
                )),
                None,
                Some(("https://example.com/css/d.eot".to_string(), Kw::Truetype)),
            ]
        );
    }

    #[test]
    fn italic_maps_to_italic() {
        assert_eq!(stylo_to_fontique_style(&FontStyleRange::Italic), Fq::Italic,);
    }

    #[test]
    fn oblique_zero_zero_maps_to_normal() {
        // Stylo parses bare CSS `normal` as `Oblique(0deg, 0deg)`; the
        // helper must round-trip that back to `FontStyle::Normal` so
        // parley's matching doesn't misclassify upright fonts.
        assert_eq!(stylo_to_fontique_style(&oblique(0.0, 0.0)), Fq::Normal);
    }

    #[test]
    fn oblique_single_angle_maps_to_oblique_with_min() {
        assert_eq!(
            stylo_to_fontique_style(&oblique(14.0, 14.0)),
            Fq::Oblique(Some(14.0)),
        );
    }

    #[test]
    fn oblique_range_uses_min_angle() {
        // For a range, fontique's single-angle representation takes the
        // lower bound — confirm `min` (not `max`) is what gets through.
        assert_eq!(
            stylo_to_fontique_style(&oblique(10.0, 20.0)),
            Fq::Oblique(Some(10.0)),
        );
    }
}
