use blitz_shell::BlitzShellProxy;
use std::sync::Arc;

use blitz_traits::net::{NetHandler, NetProvider, Request};

/// Decides whether a document's request is made: see [`crate::Config::with_request_filter`].
pub(crate) type RequestFilter = Arc<dyn Fn(&Request) -> bool + Send + Sync>;

/// The app's own providers, by URL scheme: see [`crate::Config::with_protocol`].
pub(crate) type Protocols = std::collections::HashMap<String, Arc<dyn NetProvider>>;

pub struct DioxusNativeNetProvider {
    inner_net_provider: Option<Arc<dyn NetProvider + 'static>>,
    filter: Option<RequestFilter>,
    protocols: Protocols,
}

#[allow(unused)]
impl DioxusNativeNetProvider {
    pub fn shared(proxy: BlitzShellProxy) -> Arc<dyn NetProvider> {
        Arc::new(Self::new(proxy)) as Arc<dyn NetProvider>
    }

    pub fn new(proxy: BlitzShellProxy) -> Self {
        #[cfg(any(feature = "data-uri", feature = "net"))]
        let net_waker = Some(Arc::new(proxy) as _);

        #[cfg(feature = "net")]
        let inner_net_provider = Some(blitz_net::Provider::shared(net_waker.clone()));
        #[cfg(all(feature = "data-uri", not(feature = "net")))]
        let inner_net_provider = Some(blitz_shell::DataUriNetProvider::shared(net_waker.clone()));
        #[cfg(all(not(feature = "data-uri"), not(feature = "net")))]
        let inner_net_provider = None;

        Self {
            inner_net_provider,
            filter: None,
            protocols: Protocols::new(),
        }
    }

    pub fn with_inner(proxy: BlitzShellProxy, inner: Arc<dyn NetProvider>) -> Self {
        Self {
            inner_net_provider: Some(inner),
            filter: None,
            protocols: Protocols::new(),
        }
    }

    /// This provider, refusing the requests `filter` returns `false` for.
    pub(crate) fn with_filter(mut self, filter: Option<RequestFilter>) -> Self {
        self.filter = filter;
        self
    }

    /// This provider, handing the requests for these schemes to their providers.
    pub(crate) fn with_protocols(mut self, protocols: Protocols) -> Self {
        self.protocols = protocols;
        self
    }

    pub fn inner(&self) -> Option<&Arc<dyn NetProvider>> {
        self.inner_net_provider.as_ref()
    }
}

impl NetProvider for DioxusNativeNetProvider {
    fn fetch(&self, doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        if self.filter.as_ref().is_some_and(|allows| !allows(&request)) {
            #[cfg(feature = "tracing")]
            tracing::debug!(url = request.url.as_str(), "request refused by the filter");
            handler.error(request.url.to_string(), "refused".to_string());
        } else if let Some(provider) = self.protocols.get(request.url.scheme()) {
            provider.fetch(doc_id, request, handler);
        } else if request.url.scheme() == "dioxus" {
            #[allow(clippy::single_match)] // cfg'd code
            match dioxus_asset_resolver::native::serve_asset(request.url.path()) {
                Ok(res) => {
                    #[cfg(feature = "tracing")]
                    tracing::trace!("fetching asset from file system success {request:#?}");
                    handler.bytes(request.url.to_string(), res.into_body().into())
                }
                Err(_) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("fetching asset from file system error {request:#?}");
                }
            }
        } else if let Some(inner) = &self.inner_net_provider {
            inner.fetch(doc_id, request, handler);
        } else {
            #[cfg(feature = "tracing")]
            tracing::warn!("net feature not enabled, cannot fetch {request:#?}");
        }
    }
}
