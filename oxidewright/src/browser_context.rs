//! [`BrowserContext`]: an isolated browser session (cookies, storage, pages).

use std::fmt;
use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use chromiumoxide::cdp::browser_protocol::browser::BrowserContextId;
use chromiumoxide::cdp::browser_protocol::page::ScriptIdentifier;
use chromiumoxide::cdp::browser_protocol::target::{
    CreateTargetParams, DisposeBrowserContextParams,
};

use crate::browser::{Browser, BrowserInner};
use crate::error::{Error, Result, ResultExt};
use crate::page::{AddInitScript, InitScriptTarget, Page, PageInner, ViewportSize};
use crate::progress::TimeoutSettings;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Options of [`crate::Browser::new_context`] (Playwright's
/// `BrowserContextOptions`). Options left unset are not emulated, see
/// [`crate::emulation`] about stealth. Builder methods live on
/// [`NewContext`].
#[derive(Debug, Clone)]
pub struct NewContextOptions {
    /// Viewport of every page (default 1280x720); `None` keeps the window size.
    pub viewport: Option<ViewportSize>,
    /// Base URL used to resolve relative URLs of `goto`, `wait_for_url`, ...
    pub base_url: Option<String>,
    /// Extra HTTP headers sent with every request.
    pub extra_http_headers: Vec<crate::Header>,
    /// Emulate a disconnected network.
    pub offline: bool,
    /// Credentials for HTTP authentication.
    pub http_credentials: Option<crate::HttpCredentials>,
    /// Proxy of this context's requests (overrides the browser's).
    pub proxy: Option<crate::ProxySettings>,
    /// Ignore HTTPS errors.
    pub ignore_https_errors: bool,
    /// Accept downloads (default `true`).
    pub accept_downloads: bool,
    /// Record a HAR file.
    pub record_har: Option<crate::RecordHar>,
    /// Report `console`/`pageerror` events (enables `Runtime`, not stealthy).
    pub console_events: bool,
    /// Screen size exposed to pages (`window.screen`); defaults to the viewport.
    pub screen: Option<ViewportSize>,
    /// Device scale factor (`window.devicePixelRatio`), default 1.
    pub device_scale_factor: Option<f64>,
    /// Emulate a mobile device (meta viewport, overlay scrollbars...).
    pub is_mobile: Option<bool>,
    /// Emulate touch support.
    pub has_touch: Option<bool>,
    /// User agent override.
    pub user_agent: Option<String>,
    /// Locale (`navigator.language`, `Accept-Language`, `Intl`), e.g. `de-DE`.
    pub locale: Option<String>,
    /// Timezone, e.g. `Europe/Lisbon`.
    pub timezone_id: Option<String>,
    /// Emulated geolocation.
    pub geolocation: Option<crate::emulation::Geolocation>,
    /// Permissions granted to every origin.
    pub permissions: Option<Vec<String>>,
    /// `prefers-color-scheme`.
    pub color_scheme: Option<crate::emulation::ColorScheme>,
    /// `prefers-reduced-motion`.
    pub reduced_motion: Option<crate::emulation::ReducedMotion>,
    /// `forced-colors`.
    pub forced_colors: Option<crate::emulation::ForcedColors>,
    /// `prefers-contrast`.
    pub contrast: Option<crate::emulation::Contrast>,
    /// `Some(false)` disables JavaScript.
    pub java_script_enabled: Option<bool>,
    /// Bypass the page's Content-Security-Policy.
    pub bypass_csp: bool,
    /// Make locators strict by default.
    pub strict_selectors: bool,
    /// Whether pages may register service workers.
    pub service_workers: crate::emulation::ServiceWorkerPolicy,
    /// Initial cookies and storage.
    pub storage_state: Option<crate::storage_state::StorageStateSource>,
    /// Record a video of every page into this directory (requires ffmpeg).
    pub record_video_dir: Option<std::path::PathBuf>,
    /// Size of the recorded videos.
    pub record_video_size: Option<ViewportSize>,
}

impl Default for NewContextOptions {
    fn default() -> Self {
        Self {
            viewport: Some(ViewportSize {
                width: 1280,
                height: 720,
            }),
            ..Self::no_overrides()
        }
    }
}

impl NewContextOptions {
    /// Options that emulate nothing, used for the default context of a
    /// browser attached with `connect_over_cdp`.
    pub fn no_overrides() -> Self {
        Self {
            viewport: None,
            base_url: None,
            extra_http_headers: Vec::new(),
            offline: false,
            http_credentials: None,
            proxy: None,
            ignore_https_errors: false,
            accept_downloads: true,
            record_har: None,
            console_events: false,
            screen: None,
            device_scale_factor: None,
            is_mobile: None,
            has_touch: None,
            user_agent: None,
            locale: None,
            timezone_id: None,
            geolocation: None,
            permissions: None,
            color_scheme: None,
            reduced_motion: None,
            forced_colors: None,
            contrast: None,
            java_script_enabled: None,
            bypass_csp: false,
            strict_selectors: false,
            service_workers: Default::default(),
            storage_state: None,
            record_video_dir: None,
            record_video_size: None,
        }
    }
}

/// An isolated browser context. Cheap to clone.
#[derive(Clone)]
pub struct BrowserContext {
    pub(crate) inner: Arc<ContextInner>,
}

pub(crate) struct ContextInner {
    pub(crate) id: Option<BrowserContextId>,
    pub(crate) browser: Weak<BrowserInner>,
    pub(crate) options: NewContextOptions,
    pub(crate) timeouts: Arc<TimeoutSettings>,
    /// Events, routes, network options (see `context_ext.rs`).
    pub(crate) ext: crate::context_ext::ContextExt,
    pub(crate) pages: Mutex<Vec<Page>>,
    init_scripts: Mutex<Vec<String>>,
    closed: AtomicBool,
    pub(crate) emu: crate::ext::ContextExt,
    /// The page of `browser.new_page()` owning this context.
    owner_page: Mutex<Option<Weak<PageInner>>>,
    /// The browser of a persistent context, kept alive by it (the caller
    /// only holds the context); released when the browser closes.
    kept_browser: Mutex<Option<Browser>>,
    /// The simulated person using this context (`BrowserContext::human`),
    /// dropped when the context closes.
    pub(crate) human: Mutex<Option<crate::human::Human>>,
}

impl fmt::Debug for BrowserContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserContext")
            .field("id", &self.inner.id)
            .finish()
    }
}

impl PartialEq for BrowserContext {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl ContextInner {
    pub(crate) fn remove_page(&self, page: &PageInner) {
        self.pages
            .lock()
            .unwrap()
            .retain(|p| !std::ptr::eq(Arc::as_ptr(&p.inner), page));
    }
}

impl BrowserContext {
    pub(crate) fn new(
        id: Option<BrowserContextId>,
        browser: Weak<BrowserInner>,
        options: NewContextOptions,
    ) -> Self {
        BrowserContext {
            inner: Arc::new(ContextInner {
                id,
                browser,
                ext: crate::context_ext::ContextExt::new(&options),
                options,
                timeouts: Arc::new(TimeoutSettings::new(None)),
                pages: Mutex::new(Vec::new()),
                init_scripts: Mutex::new(Vec::new()),
                closed: AtomicBool::new(false),
                emu: Default::default(),
                owner_page: Mutex::new(None),
                kept_browser: Mutex::new(None),
                human: Mutex::new(None),
            }),
        }
    }

    fn browser_inner(&self) -> Result<Arc<BrowserInner>> {
        self.inner
            .browser
            .upgrade()
            .ok_or_else(Error::target_closed)
    }

    /// The browser owning this context.
    pub fn browser(&self) -> Option<Browser> {
        self.inner.browser.upgrade().map(|inner| Browser { inner })
    }

    /// Open pages of the context.
    pub fn pages(&self) -> Vec<Page> {
        self.inner
            .pages
            .lock()
            .unwrap()
            .iter()
            .filter(|p| !p.is_closed())
            .cloned()
            .collect()
    }

    /// Wrap a chromiumoxide page of this context and track it.
    pub(crate) async fn adopt_page(&self, cdp: chromiumoxide::Page) -> Result<Page> {
        self.adopt_page_with_opener(cdp, None).await
    }

    /// Like `adopt_page`, for a popup of `opener`.
    pub(crate) async fn adopt_page_with_opener(
        &self,
        cdp: chromiumoxide::Page,
        opener: Option<Weak<PageInner>>,
    ) -> Result<Page> {
        let browser = self.browser_inner()?;
        let target_id = cdp.target_id().as_ref().to_string();
        if let Some(inner) = browser.page_by_target(&target_id) {
            return Ok(Page::from_inner(inner));
        }
        // `new_page` and the target listener may both adopt the same page:
        // the first one does it, the other gets the same page.
        let cell = browser.adoption_cell(&target_id);
        let result = cell
            .get_or_try_init(|| self.adopt_page_once(cdp, opener))
            .await
            .cloned();
        browser.finish_adoption(&target_id);
        result
    }

    async fn adopt_page_once(
        &self,
        cdp: chromiumoxide::Page,
        opener: Option<Weak<PageInner>>,
    ) -> Result<Page> {
        let browser = self.browser_inner()?;
        let scripts = self.inner.init_scripts.lock().unwrap().clone();
        let scripts_installed = crate::emulation::preconfigure_new_page(&cdp, self, &scripts).await;
        let inner = PageInner::attach(cdp, Arc::downgrade(&browser), self).await?;
        *inner.ext.opener.lock().unwrap() = opener;
        let page = Page::from_inner(inner);
        crate::emulation::init_page(&page).await?;
        crate::screencast::on_page_created(&page).await?;
        crate::binding::init_page(&page, self).await?;
        crate::credentials::init_page(&page, self).await?;
        if !scripts_installed {
            for script in scripts {
                page.inner.add_init_script_source_with(script, true).await?;
            }
        }
        crate::page_ext::initialize(&page, self).await?;
        // New pages and popups start paused (`wait_for_debugger_on_start`):
        // everything above now applies to their first document. A no-op for
        // a page that was not paused.
        let _ = page
            .inner
            .cdp
            .execute(
                chromiumoxide::cdp::js_protocol::runtime::RunIfWaitingForDebuggerParams::default(),
            )
            .await;
        browser.remember_page(&page);
        self.inner.pages.lock().unwrap().push(page.clone());
        self.inner
            .ext
            .listeners
            .emit(crate::events::Event::Page(page.clone()));
        Ok(page)
    }

    /// Open a new page in this context.
    pub async fn new_page(&self) -> Result<Page> {
        async {
            if self.inner.closed.load(Ordering::SeqCst) {
                return Err(Error::target_closed());
            }
            if self.inner.owner_page.lock().unwrap().is_some() {
                return Err(Error::invalid_argument("Please use browser.newContext()"));
            }
            let browser = self.browser_inner()?;
            let mut params = CreateTargetParams::new("about:blank");
            params.browser_context_id = self.inner.id.clone();
            // Each page gets its own window (like Playwright's pages): a background
            // tab of a shared window would not render, so screenshots would stall.
            params.new_window = Some(true);
            let cdp = browser.new_cdp_page(params).await?;
            self.adopt_page(cdp).await
        }
        .await
        .api("browserContext.newPage")
    }

    /// Default timeout (ms) of actions of every page of the context.
    pub fn set_default_timeout(&self, timeout_ms: u64) {
        self.inner.timeouts.set_default_timeout(timeout_ms);
    }

    /// Default timeout (ms) of navigations of every page of the context.
    pub fn set_default_navigation_timeout(&self, timeout_ms: u64) {
        self.inner
            .timeouts
            .set_default_navigation_timeout(timeout_ms);
    }

    /// Add a script evaluated in every new document of every page.
    pub fn add_init_script(&self, script: impl Into<String>) -> AddInitScript {
        AddInitScript {
            target: InitScriptTarget::Context(self.clone()),
            script: script.into(),
            arg: None,
        }
    }

    pub(crate) async fn add_init_script_source(
        &self,
        source: String,
    ) -> Result<Vec<(Weak<PageInner>, ScriptIdentifier)>> {
        self.inner.init_scripts.lock().unwrap().push(source.clone());
        let mut ids = Vec::new();
        for page in self.pages() {
            ids.push(page.inner.add_init_script_source(source.clone()).await?);
        }
        Ok(ids)
    }

    pub(crate) fn remove_init_script_source(&self, source: &str) {
        let mut scripts = self.inner.init_scripts.lock().unwrap();
        if let Some(index) = scripts.iter().position(|s| s == source) {
            scripts.remove(index);
        }
    }

    /// Whether the context has been closed.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Close the context and all its pages; `.reason(..)` sets the message
    /// of errors of operations interrupted by the closure.
    pub fn close(&self) -> crate::page_extras::CloseContext {
        crate::page_extras::CloseContext {
            context: self.clone(),
            reason: None,
        }
    }

    /// The context of `browser.new_page()` owned by `page`, if any.
    pub(crate) fn owned_by(page: &Page) -> Option<BrowserContext> {
        let context = page.context()?;
        let owner = context.inner.owner_page.lock().unwrap().clone()?;
        std::ptr::eq(owner.as_ptr(), Arc::as_ptr(&page.inner)).then(|| context.clone())
    }

    /// Whether this is the persistent context of
    /// `launch_persistent_context` (closing it closes the browser).
    fn is_persistent(&self) -> bool {
        self.inner.id.is_none()
            && self
                .inner
                .browser
                .upgrade()
                .is_some_and(|b| b.persistent.load(Ordering::SeqCst))
    }

    /// Keep the browser of a persistent context alive.
    pub(crate) fn keep_browser(&self, browser: Browser) {
        *self.inner.kept_browser.lock().unwrap() = Some(browser);
    }

    /// The browser closed: close the context without talking to it.
    pub(crate) fn did_close_with_browser(&self) {
        let kept = self.inner.kept_browser.lock().unwrap().take();
        drop(kept);
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let pages = std::mem::take(&mut *self.inner.pages.lock().unwrap());
        for page in &pages {
            page.inner.did_close();
        }
        if let Some(browser) = self.inner.browser.upgrade() {
            browser.forget_context(self);
        }
        self.emit_closed();
    }

    pub(crate) async fn close_impl(&self) -> Result<()> {
        crate::video::on_context_closing(self).await;
        async {
            if self.inner.closed.load(Ordering::SeqCst) {
                return Ok(());
            }
            let har = crate::har::flush_context(self).await;
            if self.is_persistent() {
                // Like Playwright, closing the persistent context closes the
                // browser (which closes this context).
                if let Some(browser) = self.browser() {
                    browser.close().await?;
                }
                self.did_close_with_browser();
                return har;
            }
            if self.inner.closed.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            let pages = std::mem::take(&mut *self.inner.pages.lock().unwrap());
            let browser = self.inner.browser.upgrade();
            match (&self.inner.id, &browser) {
                (Some(id), Some(browser)) if browser.is_connected() => {
                    let _ = browser
                        .execute_with_session(DisposeBrowserContextParams::new(id.clone()), None)
                        .await;
                }
                _ => {
                    for page in &pages {
                        let _ = page.close().await;
                    }
                }
            }
            for page in &pages {
                page.inner.did_close();
            }
            if let Some(browser) = browser {
                browser.forget_context(self);
            }
            self.emit_closed();
            har
        }
        .await
        .api("browserContext.close")
    }
}

/// Builder returned by [`crate::Browser::new_context`]; `.await` it.
#[must_use = "builders do nothing unless awaited"]
pub struct NewContext {
    /// `None` for the context options of `launch_persistent_context`.
    pub(crate) browser: Option<Browser>,
    pub(crate) options: NewContextOptions,
}

impl NewContext {
    /// Viewport of every page; `None` disables viewport emulation.
    pub fn viewport(mut self, viewport: Option<ViewportSize>) -> Self {
        self.options.viewport = viewport;
        self
    }

    /// Base URL used to resolve relative URLs.
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.options.base_url = Some(base_url.into());
        self
    }

    /// Replace all options at once.
    pub fn options(mut self, options: NewContextOptions) -> Self {
        self.options = options;
        self
    }
}

impl IntoFuture for NewContext {
    type Output = Result<BrowserContext>;
    type IntoFuture = BoxFuture<Result<BrowserContext>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            async {
                let browser = self.browser.ok_or_else(Error::target_closed)?;
                crate::emulation::validate_options(&self.options)?;
                crate::video::validate_options(&self.options)?;
                let context = browser.inner.create_context(self.options).await?;
                if let Err(err) = crate::emulation::init_context(&context).await {
                    let _ = context.close().await;
                    return Err(err);
                }
                browser
                    .inner
                    .listeners
                    .emit(crate::events::Event::Context(context.clone()));
                Ok(context)
            }
            .await
            .api("browser.newContext")
        })
    }
}

/// Builder returned by [`crate::Browser::new_page`]; `.await` it. Takes every
/// option of [`NewContext`].
#[must_use = "builders do nothing unless awaited"]
pub struct NewPage {
    pub(crate) context: NewContext,
}

impl IntoFuture for NewPage {
    type Output = Result<Page>;
    type IntoFuture = BoxFuture<Result<Page>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let context = self
                .context
                .await
                .map_err(|e| e.with_api("browser.newPage"))?;
            let page = match context.new_page().await {
                Ok(page) => page,
                Err(err) => {
                    let _ = context.close().await;
                    return Err(err.with_api("browser.newPage"));
                }
            };
            *context.inner.owner_page.lock().unwrap() = Some(Arc::downgrade(&page.inner));
            Ok(page)
        })
    }
}

/// The builder methods of [`NewContext`] on a builder with a `NewContext`
/// field, so `browser.new_page()` and `launch_persistent_context()` take
/// every context option.
macro_rules! forward_context_options {
    ($field:ident) => {
        /// Viewport of every page; `None` disables viewport emulation.
        pub fn viewport(mut self, viewport: Option<$crate::ViewportSize>) -> Self {
            self.$field = self.$field.viewport(viewport);
            self
        }
        /// Base URL used to resolve relative URLs.
        pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
            self.$field = self.$field.base_url(base_url);
            self
        }
        /// Replace all context options at once.
        pub fn context_options(mut self, options: $crate::NewContextOptions) -> Self {
            self.$field = self.$field.options(options);
            self
        }
        /// Extra HTTP headers sent with every request.
        pub fn extra_http_headers<K: Into<String>, V: Into<String>>(
            mut self,
            headers: impl IntoIterator<Item = (K, V)>,
        ) -> Self {
            self.$field = self.$field.extra_http_headers(headers);
            self
        }
        /// Emulate a disconnected network.
        pub fn offline(mut self, offline: bool) -> Self {
            self.$field = self.$field.offline(offline);
            self
        }
        /// Credentials for HTTP authentication.
        pub fn http_credentials(mut self, credentials: $crate::HttpCredentials) -> Self {
            self.$field = self.$field.http_credentials(credentials);
            self
        }
        /// Ignore HTTPS errors (self-signed certificates, ...).
        pub fn ignore_https_errors(mut self, ignore: bool) -> Self {
            self.$field = self.$field.ignore_https_errors(ignore);
            self
        }
        /// Whether downloads are accepted (default `true`).
        pub fn accept_downloads(mut self, accept: bool) -> Self {
            self.$field = self.$field.accept_downloads(accept);
            self
        }
        /// Record a HAR file of the context, written when it closes.
        pub fn record_har(mut self, record_har: $crate::RecordHar) -> Self {
            self.$field = self.$field.record_har(record_har);
            self
        }
        /// Report `console` and `pageerror` events (not stealthy, see
        /// [`crate::NewContext::console_events`]).
        pub fn console_events(mut self, enabled: bool) -> Self {
            self.$field = self.$field.console_events(enabled);
            self
        }
        /// Emulate a device (see [`crate::devices`]).
        pub fn device(mut self, device: &$crate::Device) -> Self {
            self.$field = self.$field.device(device);
            self
        }
        /// Screen size exposed to pages (`window.screen`).
        pub fn screen(mut self, screen: impl Into<$crate::ViewportSize>) -> Self {
            self.$field = self.$field.screen(screen);
            self
        }
        /// Device scale factor (`window.devicePixelRatio`).
        pub fn device_scale_factor(mut self, factor: f64) -> Self {
            self.$field = self.$field.device_scale_factor(factor);
            self
        }
        /// Emulate a mobile device.
        pub fn is_mobile(mut self, is_mobile: bool) -> Self {
            self.$field = self.$field.is_mobile(is_mobile);
            self
        }
        /// Emulate touch support.
        pub fn has_touch(mut self, has_touch: bool) -> Self {
            self.$field = self.$field.has_touch(has_touch);
            self
        }
        /// User agent override.
        pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
            self.$field = self.$field.user_agent(user_agent);
            self
        }
        /// Locale, e.g. `de-DE`.
        pub fn locale(mut self, locale: impl Into<String>) -> Self {
            self.$field = self.$field.locale(locale);
            self
        }
        /// Timezone, e.g. `Europe/Lisbon`.
        pub fn timezone_id(mut self, timezone_id: impl Into<String>) -> Self {
            self.$field = self.$field.timezone_id(timezone_id);
            self
        }
        /// Emulated geolocation.
        pub fn geolocation(mut self, geolocation: $crate::emulation::Geolocation) -> Self {
            self.$field = self.$field.geolocation(geolocation);
            self
        }
        /// Permissions granted to every origin.
        pub fn permissions<I, S>(mut self, permissions: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<String>,
        {
            self.$field = self.$field.permissions(permissions);
            self
        }
        /// `prefers-color-scheme`.
        pub fn color_scheme(mut self, value: $crate::emulation::ColorScheme) -> Self {
            self.$field = self.$field.color_scheme(value);
            self
        }
        /// `prefers-reduced-motion`.
        pub fn reduced_motion(mut self, value: $crate::emulation::ReducedMotion) -> Self {
            self.$field = self.$field.reduced_motion(value);
            self
        }
        /// `forced-colors`.
        pub fn forced_colors(mut self, value: $crate::emulation::ForcedColors) -> Self {
            self.$field = self.$field.forced_colors(value);
            self
        }
        /// `prefers-contrast`.
        pub fn contrast(mut self, value: $crate::emulation::Contrast) -> Self {
            self.$field = self.$field.contrast(value);
            self
        }
        /// `false` disables JavaScript.
        pub fn java_script_enabled(mut self, enabled: bool) -> Self {
            self.$field = self.$field.java_script_enabled(enabled);
            self
        }
        /// Bypass the page's Content-Security-Policy.
        pub fn bypass_csp(mut self, bypass: bool) -> Self {
            self.$field = self.$field.bypass_csp(bypass);
            self
        }
        /// Make locators strict by default.
        pub fn strict_selectors(mut self, strict: bool) -> Self {
            self.$field = self.$field.strict_selectors(strict);
            self
        }
        /// Whether pages may register service workers.
        pub fn service_workers(mut self, policy: $crate::emulation::ServiceWorkerPolicy) -> Self {
            self.$field = self.$field.service_workers(policy);
            self
        }
        /// Initial cookies and storage.
        pub fn storage_state(mut self, state: impl Into<$crate::StorageStateSource>) -> Self {
            self.$field = self.$field.storage_state(state);
            self
        }
        /// Record a video of every page into `dir` (requires ffmpeg).
        pub fn record_video_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
            self.$field = self.$field.record_video_dir(dir);
            self
        }
        /// Size of the recorded videos.
        pub fn record_video_size(mut self, size: impl Into<$crate::ViewportSize>) -> Self {
            self.$field = self.$field.record_video_size(size);
            self
        }
    };
}
pub(crate) use forward_context_options;

impl NewPage {
    forward_context_options!(context);

    /// Proxy of the page's context (overrides the browser's).
    pub fn proxy(mut self, proxy: crate::ProxySettings) -> Self {
        self.context = self.context.proxy(proxy);
        self
    }
}
