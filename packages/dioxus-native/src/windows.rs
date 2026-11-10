//! Opening more windows from a running app, as Dioxus Desktop's
//! `use_window().new_window(dom, cfg)` does.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use blitz_dom::{DocumentConfig, HtmlParserProvider, StyleThreading};
use blitz_shell::{BlitzShellEvent, BlitzShellProxy, WindowConfig};
use blitz_traits::net::NetProvider;
use dioxus_core::{VirtualDom, consume_context, use_hook};
use winit::window::WindowId;

use crate::link_handler::DioxusNativeNavigationProvider;
use crate::{
    Config, DioxusDocument, DioxusNativeEvent, DioxusNativeWindowRenderer, RendererOptions,
};

/// Opens and closes the app's windows. Every window has it as a context.
///
/// Each window runs its own [`VirtualDom`]: signals don't cross between windows, so
/// share state through a context given to the new window's `VirtualDom`.
#[derive(Clone)]
pub struct Windows {
    pending: Rc<RefCell<Vec<(VirtualDom, Config)>>>,
    proxy: BlitzShellProxy,
}

impl Windows {
    pub(crate) fn new(proxy: BlitzShellProxy) -> Self {
        Self {
            pending: Rc::default(),
            proxy,
        }
    }

    /// Opens a window that renders `vdom`, set up as `config` says. It opens once the
    /// event loop next runs.
    pub fn new_window(&self, vdom: VirtualDom, config: Config) {
        self.pending.borrow_mut().push((vdom, config));
        self.proxy.send_event(BlitzShellEvent::embedder_event(
            DioxusNativeEvent::OpenWindows,
        ));
    }

    /// Closes the window, as its close button does.
    pub fn close_window(&self, window_id: WindowId) {
        self.proxy
            .send_event(BlitzShellEvent::CloseWindow { window_id });
    }

    /// The windows asked for since the last call.
    pub(crate) fn take_pending(&self) -> Vec<(VirtualDom, Config)> {
        std::mem::take(&mut *self.pending.borrow_mut())
    }
}

/// The app's [`Windows`].
pub fn use_windows() -> Windows {
    use_hook(consume_context::<Windows>)
}

/// What every window's document and renderer are made with, so a window opened later
/// gets what the first one got.
pub(crate) struct WindowFactory {
    pub(crate) net_provider: Arc<dyn NetProvider>,
    #[cfg(all(feature = "net", not(target_arch = "wasm32")))]
    pub(crate) inner_net_provider: Arc<blitz_net::Provider>,
    pub(crate) html_parser_provider: Option<Arc<dyn HtmlParserProvider>>,
    #[cfg(any(feature = "vello", feature = "vello-hybrid"))]
    pub(crate) features: Option<crate::Features>,
    #[cfg(any(feature = "vello", feature = "vello-hybrid"))]
    pub(crate) limits: Option<crate::Limits>,
}

impl WindowFactory {
    /// The window for `vdom`, with the providers it needs as root contexts.
    pub(crate) fn build(
        &self,
        vdom: VirtualDom,
        config: Config,
    ) -> WindowConfig<DioxusNativeWindowRenderer> {
        #[cfg(all(feature = "net", not(target_arch = "wasm32")))]
        vdom.provide_root_context(Arc::clone(&self.inner_net_provider));
        vdom.provide_root_context(Arc::clone(&self.net_provider));
        if let Some(html_parser) = &self.html_parser_provider {
            vdom.provide_root_context(Arc::clone(html_parser));
        }

        let doc = DioxusDocument::new(
            vdom,
            DocumentConfig {
                net_provider: Some(Arc::clone(&self.net_provider)),
                html_parser_provider: self.html_parser_provider.clone(),
                navigation_provider: Some(Arc::new(DioxusNativeNavigationProvider) as _),
                font_ctx: config.font_ctx,
                style_threading: StyleThreading::Parallel,
                ..Default::default()
            },
        );
        let renderer = DioxusNativeWindowRenderer::with_options(RendererOptions {
            base_color: config.base_color,
            alpha_mode: config.alpha_mode,
            #[cfg(any(feature = "vello", feature = "vello-hybrid"))]
            features: self.features,
            #[cfg(any(feature = "vello", feature = "vello-hybrid"))]
            limits: self.limits.clone(),
        });
        WindowConfig::with_attributes(Box::new(doc) as _, renderer, config.window_attributes)
    }
}
