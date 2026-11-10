use anyrender::CompositeAlphaMode;
use blitz_dom::FontContext;
use dioxus_core::LaunchConfig;
use peniko::Color;
use winit::window::WindowAttributes;

use crate::KeyboardShortcut;
use crate::assets::{Protocols, RequestFilter};

/// Launch-time configuration for a dioxus-native application.
pub struct Config {
    pub(crate) window_attributes: WindowAttributes,
    pub(crate) font_ctx: Option<FontContext>,
    pub(crate) alpha_mode: Option<CompositeAlphaMode>,
    pub(crate) base_color: Option<Color>,
    #[cfg_attr(not(feature = "devtools"), allow(dead_code))]
    pub(crate) devtools_shortcuts: Vec<KeyboardShortcut>,
    pub(crate) request_filter: Option<RequestFilter>,
    pub(crate) protocols: Protocols,
    #[cfg(all(feature = "net", not(target_arch = "wasm32")))]
    pub(crate) http_client: Option<blitz_net::reqwest::Client>,
}

impl LaunchConfig for Config {}

impl Default for Config {
    fn default() -> Self {
        let window_attributes = WindowAttributes::default()
            .with_title(dioxus_cli_config::app_title().unwrap_or_else(|| "Dioxus App".to_string()));

        // On WASM, append the canvas to the document body. Surface size is
        // intentionally not seeded: winit-web's with_surface_size writes inline
        // canvas.style.width/height that overrides host CSS. View::init reads
        // the canvas's CSS layout box when winit reports a 0×0 initial size.
        // To target an existing `<canvas>`, replace these attributes via
        // [`Config::with_window_attributes`].
        #[cfg(target_arch = "wasm32")]
        let window_attributes = {
            use winit::platform::web::WindowAttributesWeb;
            window_attributes.with_platform_attributes(Box::new(
                WindowAttributesWeb::default().with_append(true),
            ))
        };

        Self {
            window_attributes,
            font_ctx: None,
            alpha_mode: None,
            base_color: None,
            devtools_shortcuts: if cfg!(feature = "devtools") {
                KeyboardShortcut::chrome_devtools().to_vec()
            } else {
                Vec::new()
            },
            request_filter: None,
            protocols: Protocols::new(),
            #[cfg(all(feature = "net", not(target_arch = "wasm32")))]
            http_client: None,
        }
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the configuration for the window.
    pub fn with_window_attributes(mut self, attrs: WindowAttributes) -> Self {
        self.window_attributes = attrs;
        self
    }

    /// Set a custom [`FontContext`] for the document.
    ///
    /// On WASM, browsers don't expose system fonts, so a `FontContext` with
    /// bundled fonts must be provided. Use [`crate::build_single_font_ctx`] for
    /// the common one-font case.
    pub fn with_font_ctx(mut self, font_ctx: FontContext) -> Self {
        self.font_ctx = Some(font_ctx);
        self
    }

    /// Set the alpha mode used when compositing the window surface.
    ///
    /// This controls how the rendered content is blended with the background,
    /// e.g. to enable transparent windows. Not all renderers support every
    /// mode; unsupported modes are ignored by the active renderer.
    pub fn with_alpha_mode(mut self, alpha_mode: CompositeAlphaMode) -> Self {
        self.alpha_mode = Some(alpha_mode);
        self
    }

    /// Set a filter that every document's requests pass first, whatever their
    /// scheme: a request it returns `false` for fails without being made. For
    /// documents with untrusted content, such as to refuse `file:` URLs.
    pub fn with_request_filter(
        mut self,
        filter: impl Fn(&blitz_traits::net::Request) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.request_filter = Some(std::sync::Arc::new(filter));
        self
    }

    /// Serve every document's requests for URLs of `scheme` with `provider`, such
    /// as images the app keeps itself. The request filter still comes first, so it
    /// must let the scheme through.
    ///
    /// The provider must answer every request, through the handler's `bytes` or
    /// `error`, on its own thread: one left unanswered leaves its elements waiting.
    /// Subdocuments, such as an `<iframe srcdoc>`'s, use the same providers, so
    /// untrusted content can request the scheme's URLs too.
    pub fn with_protocol(
        mut self,
        scheme: impl Into<String>,
        provider: std::sync::Arc<dyn blitz_traits::net::NetProvider>,
    ) -> Self {
        // A parsed URL's scheme is lowercase.
        let scheme = scheme.into().to_ascii_lowercase();
        self.protocols.insert(scheme, provider);
        self
    }

    /// Set the client that documents' HTTP requests go through, instead of a
    /// default one: such as one whose DNS resolver refuses some addresses.
    #[cfg(all(feature = "net", not(target_arch = "wasm32")))]
    pub fn with_http_client(mut self, client: blitz_net::reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Set the base (background) color used to clear each frame.
    pub fn with_base_color(mut self, base_color: Color) -> Self {
        self.base_color = Some(base_color);
        self
    }

    /// Set the keyboard shortcuts that open devtools inspecting a window's
    /// document when pressed in it (replacing any set before). The key press
    /// isn't delivered to the app.
    ///
    /// With the `devtools` feature, this defaults to Chrome's devtools
    /// shortcuts ([`KeyboardShortcut::chrome_devtools`]). Pass an empty list to
    /// disable them:
    ///
    /// ```no_run
    /// # use dioxus_native::{Config, KeyboardShortcut};
    /// # use dioxus_native::winit::keyboard::{KeyCode, ModifiersState};
    /// // Only F11
    /// let config = Config::new()
    ///     .with_devtools_shortcuts([KeyboardShortcut::new(ModifiersState::empty(), KeyCode::F11)]);
    /// // None
    /// let config = Config::new().with_devtools_shortcuts([]);
    /// ```
    ///
    /// Shortcuts do nothing without the `devtools` feature. With the
    /// `embedded-devtools-frontend` feature too, devtools open in a browser;
    /// otherwise they open Chrome's own devtools frontend, which only works
    /// where Chrome can be launched directly (e.g. macOS). Elsewhere, a
    /// `devtools://` URL to paste into Chrome is printed.
    pub fn with_devtools_shortcuts(
        mut self,
        shortcuts: impl IntoIterator<Item = KeyboardShortcut>,
    ) -> Self {
        self.devtools_shortcuts = shortcuts.into_iter().collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devtools_shortcuts_default_to_chrome_with_the_devtools_feature() {
        let expected = if cfg!(feature = "devtools") {
            KeyboardShortcut::chrome_devtools().to_vec()
        } else {
            Vec::new()
        };
        assert_eq!(Config::new().devtools_shortcuts, expected);
    }

    #[test]
    fn with_devtools_shortcuts_replaces_them() {
        let f11 = KeyboardShortcut::new(Default::default(), winit::keyboard::KeyCode::F11);
        assert_eq!(
            Config::new()
                .with_devtools_shortcuts([f11])
                .devtools_shortcuts,
            vec![f11]
        );
        assert!(
            Config::new()
                .with_devtools_shortcuts([])
                .devtools_shortcuts
                .is_empty()
        );
    }
}
