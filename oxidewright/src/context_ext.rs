//! Context-level state of the events, network and routing API and the
//! public event methods of [`BrowserContext`].

use std::future::IntoFuture;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::browser_context::{BrowserContext, NewContext, NewContextOptions};
use crate::console::{ConsoleMessage, WebError};
use crate::dialog::Dialog;
use crate::download::Download;
use crate::error::{Result, ResultExt};
use crate::events::{self, Event, EventKind, ListenerId, Listeners, WaitForEvent, typed_handlers};
use crate::frame::Frame;
use crate::har::{HarRecorder, RecordHar};
use crate::network::{Header, Request, Response};
use crate::network_manager::{HttpCredentials, NetworkManager};
use crate::page::Page;
use crate::page_ext::to_headers;
use crate::route::RouteHandler;
use crate::url_match::UrlMatch;

/// Extra state of a context.
pub(crate) struct ContextExt {
    pub(crate) listeners: Listeners,
    pub(crate) routes: Mutex<Vec<Arc<RouteHandler>>>,
    pub(crate) extra_headers: Mutex<Vec<Header>>,
    pub(crate) offline: AtomicBool,
    pub(crate) http_credentials: Mutex<Option<HttpCredentials>>,
    pub(crate) har_recorders: Mutex<Vec<Arc<HarRecorder>>>,
    /// `expose_function` / `expose_binding` of the context.
    pub(crate) bindings: crate::binding::Bindings,
    /// `route_web_socket` handlers of the context.
    pub(crate) ws_routes: crate::web_socket_route::WsRoutes,
    /// Routed WebSockets and their binding.
    pub(crate) ws_state: crate::web_socket_route::WsContextState,
    /// `context.credentials`.
    pub(crate) credentials: crate::credentials::CredentialsState,
    /// The recorder of `tracing.start_har`.
    pub(crate) tracing_har: Mutex<Option<Arc<HarRecorder>>>,
    /// `context.service_workers()`.
    pub(crate) service_workers: crate::service_worker::ServiceWorkers,
}

impl ContextExt {
    pub(crate) fn new(options: &NewContextOptions) -> Self {
        ContextExt {
            listeners: Listeners::new(),
            routes: Mutex::new(Vec::new()),
            extra_headers: Mutex::new(options.extra_http_headers.clone()),
            offline: AtomicBool::new(options.offline),
            http_credentials: Mutex::new(options.http_credentials.clone()),
            har_recorders: Mutex::new(Vec::new()),
            bindings: Default::default(),
            ws_routes: Default::default(),
            ws_state: Default::default(),
            credentials: Default::default(),
            tracing_har: Mutex::new(None),
            service_workers: Default::default(),
        }
    }

    /// Feed an event to the HAR recorders.
    pub(crate) fn har_record(&self, page: &crate::page::PageInner, event: &Event) {
        let recorders = self.har_recorders.lock().unwrap().clone();
        for recorder in recorders {
            recorder.on_event(page, event);
        }
    }

    /// `domcontentloaded`/`load` of a page, for the HAR page timings.
    pub(crate) fn har_page_event(&self, page: &Page, load: bool) {
        let recorders = self.har_recorders.lock().unwrap().clone();
        for recorder in recorders {
            recorder.on_page_lifecycle(page, load);
        }
    }
}

impl NewContext {
    /// Extra HTTP headers sent with every request.
    pub fn extra_http_headers<K: Into<String>, V: Into<String>>(
        mut self,
        headers: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.options.extra_http_headers = to_headers(headers);
        self
    }

    /// Emulate a disconnected network.
    pub fn offline(mut self, offline: bool) -> Self {
        self.options.offline = offline;
        self
    }

    /// Credentials for HTTP authentication.
    pub fn http_credentials(mut self, credentials: HttpCredentials) -> Self {
        self.options.http_credentials = Some(credentials);
        self
    }

    /// Proxy of this context's requests, overriding the browser's
    /// ([`crate::Launch::proxy`]). `server` is `http://host:port`,
    /// `socks5://host:port` or `host:port`; `bypass` is a comma-separated
    /// list of hosts (`.example.com` matches subdomains).
    pub fn proxy(mut self, proxy: crate::ProxySettings) -> Self {
        self.options.proxy = Some(proxy);
        self
    }

    /// Ignore HTTPS errors (self-signed certificates, ...).
    pub fn ignore_https_errors(mut self, ignore: bool) -> Self {
        self.options.ignore_https_errors = ignore;
        self
    }

    /// Whether downloads are accepted (default `true`).
    pub fn accept_downloads(mut self, accept: bool) -> Self {
        self.options.accept_downloads = accept;
        self
    }

    /// Record a HAR file of the context, written when it closes.
    pub fn record_har(mut self, record_har: RecordHar) -> Self {
        self.options.record_har = Some(record_har);
        self
    }

    /// Report `console` and `pageerror` events. **Not stealthy**: this
    /// enables the `Runtime` domain on every page of the context, which page
    /// scripts can detect (oxidewright never enables it otherwise).
    pub fn console_events(mut self, enabled: bool) -> Self {
        self.options.console_events = enabled;
        self
    }
}

impl BrowserContext {
    /// Call `handler` for every event of `kind` (context events: `page`,
    /// `close`, `request`, `response`, `requestfinished`, `requestfailed`,
    /// `console`, `dialog`, `dialogclosed`, `pageerror` (`weberror`),
    /// `download`, `frameattached`, `framedetached`, `framenavigated`,
    /// `pageclose`, `pageload`).
    pub fn on(
        &self,
        kind: EventKind,
        handler: impl Fn(Event) + Send + Sync + 'static,
    ) -> ListenerId {
        self.inner.ext.listeners.add(kind, Arc::new(handler), false)
    }

    /// Like [`BrowserContext::on`], for the next event only.
    pub fn once(
        &self,
        kind: EventKind,
        handler: impl Fn(Event) + Send + Sync + 'static,
    ) -> ListenerId {
        self.inner.ext.listeners.add(kind, Arc::new(handler), true)
    }

    /// Remove a handler.
    pub fn remove_listener(&self, id: ListenerId) -> bool {
        self.inner.ext.listeners.remove(id)
    }

    typed_handlers! {
        /// `page`: a new page in the context.
        on_page, Page, Page, Page;
        /// `console` (requires `console_events`).
        on_console, Console, Console, ConsoleMessage;
        /// `dialog`.
        on_dialog, Dialog, Dialog, Dialog;
        /// `weberror` (requires `console_events`).
        on_web_error, PageError, PageError, WebError;
        /// `request`.
        on_request, Request, Request, Request;
        /// `requestfailed`.
        on_request_failed, RequestFailed, RequestFailed, Request;
        /// `requestfinished`.
        on_request_finished, RequestFinished, RequestFinished, Request;
        /// `response`.
        on_response, Response, Response, Response;
        /// `download`.
        on_download, Download, Download, Download;
        /// `frameattached`.
        on_frame_attached, FrameAttached, FrameAttached, Frame;
        /// `framedetached`.
        on_frame_detached, FrameDetached, FrameDetached, Frame;
        /// `framenavigated`: a frame of a page of the context navigated.
        on_frame_navigated, FrameNavigated, FrameNavigated, Frame;
        /// `pageclose`: a page of the context closed.
        on_page_close, PageClose, PageClose, Page;
        /// `pageload`: the `load` event fired in a page of the context.
        on_page_load, PageLoad, PageLoad, Page;
    }

    /// Remove every handler of `kind` (all kinds when `None`).
    pub fn remove_all_listeners(&self, kind: Option<EventKind>) {
        self.inner.ext.listeners.remove_all(kind)
    }

    /// `close`: the context closed.
    pub fn on_close(&self, handler: impl Fn(BrowserContext) + Send + Sync + 'static) -> ListenerId {
        self.on(EventKind::Close, move |event| {
            if let Event::ContextClose(context) = event {
                handler(context)
            }
        })
    }

    fn emitter(&self) -> events::Emitter {
        events::Emitter::Context(self.clone())
    }

    /// Wait for the next event of `kind`.
    pub fn wait_for_event(&self, kind: EventKind) -> WaitForEvent<Event> {
        WaitForEvent::new(
            self.emitter(),
            kind,
            "browserContext.waitForEvent",
            events::as_any,
        )
        .with_log(format!("waiting for event \"{}\"", kind.name()))
    }

    /// Wait for the next page of the context.
    pub fn wait_for_page(&self) -> WaitForEvent<Page> {
        WaitForEvent::new(
            self.emitter(),
            EventKind::Page,
            "browserContext.waitForEvent",
            events::as_page,
        )
    }

    /// Wait for a request whose URL matches.
    pub fn wait_for_request(&self, url: impl Into<UrlMatch>) -> WaitForEvent<Request> {
        let url = url.into();
        let base = self.inner.options.base_url.clone();
        WaitForEvent::new(
            self.emitter(),
            EventKind::Request,
            "browserContext.waitForEvent",
            events::as_request,
        )
        .predicate(move |r: &Request| url.matches(base.as_deref(), &r.url()))
    }

    /// Wait for a response whose URL matches.
    pub fn wait_for_response(&self, url: impl Into<UrlMatch>) -> WaitForEvent<Response> {
        let url = url.into();
        let base = self.inner.options.base_url.clone();
        WaitForEvent::new(
            self.emitter(),
            EventKind::Response,
            "browserContext.waitForEvent",
            events::as_response,
        )
        .predicate(move |r: &Response| url.matches(base.as_deref(), &r.url()))
    }

    /// Wait for the next console message (requires `console_events`).
    pub fn wait_for_console_message(&self) -> WaitForEvent<ConsoleMessage> {
        WaitForEvent::new(
            self.emitter(),
            EventKind::Console,
            "browserContext.waitForEvent",
            events::as_console,
        )
    }

    /// Wait for the next dialog.
    pub fn wait_for_dialog(&self) -> WaitForEvent<Dialog> {
        WaitForEvent::new(
            self.emitter(),
            EventKind::Dialog,
            "browserContext.waitForEvent",
            events::as_dialog,
        )
    }

    /// Wait for the context to close.
    pub fn wait_for_close(&self) -> WaitForEvent<()> {
        WaitForEvent::new(
            self.emitter(),
            EventKind::Close,
            "browserContext.waitForEvent",
            events::as_close,
        )
    }

    /// Run `action` and return the next event of `kind`.
    pub async fn expect_event<R>(
        &self,
        kind: EventKind,
        action: impl IntoFuture<Output = Result<R>>,
    ) -> Result<Event> {
        self.wait_for_event(kind).run(action).await
    }

    /// Run `action` and return the page it created.
    pub async fn expect_page<R>(
        &self,
        action: impl IntoFuture<Output = Result<R>>,
    ) -> Result<Page> {
        self.wait_for_page().run(action).await
    }

    /// Run `action` and return the next console message.
    pub async fn expect_console_message<R>(
        &self,
        action: impl IntoFuture<Output = Result<R>>,
    ) -> Result<ConsoleMessage> {
        self.wait_for_console_message().run(action).await
    }

    /// Extra HTTP headers sent with every request of every page.
    pub async fn set_extra_http_headers<K: Into<String>, V: Into<String>>(
        &self,
        headers: impl IntoIterator<Item = (K, V)>,
    ) -> Result<()> {
        *self.inner.ext.extra_headers.lock().unwrap() = to_headers(headers);
        async {
            for page in self.pages() {
                NetworkManager::update_extra_headers(&page.inner).await?;
            }
            Ok(())
        }
        .await
        .api("browserContext.setExtraHTTPHeaders")
    }

    /// Emulate a disconnected network.
    pub async fn set_offline(&self, offline: bool) -> Result<()> {
        if self.inner.ext.offline.swap(offline, Ordering::SeqCst) == offline {
            return Ok(());
        }
        async {
            for page in self.pages() {
                NetworkManager::update_offline(&page.inner).await?;
            }
            Ok(())
        }
        .await
        .api("browserContext.setOffline")
    }

    /// Change the HTTP credentials (`None` disables authentication).
    pub async fn set_http_credentials(&self, credentials: Option<HttpCredentials>) -> Result<()> {
        *self.inner.ext.http_credentials.lock().unwrap() = credentials;
        self.update_interception().await;
        Ok(())
    }

    /// Emit the context `close` event (called by `close`).
    pub(crate) fn emit_closed(&self) {
        // The context's simulated person goes with it.
        let human = self.inner.human.lock().unwrap().take();
        if let Some(human) = human {
            human.forget_all();
        }
        self.inner
            .ext
            .listeners
            .emit(Event::ContextClose(self.clone()));
    }
}
