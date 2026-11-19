//! HTTP requests outside of pages: [`APIRequestContext`] (Playwright's
//! `server/fetch.ts`), implemented with `reqwest` (rustls).
//!
//! * [`request()`] creates standalone contexts with their own cookie jar
//!   (`playwright.request.newContext()`);
//! * [`BrowserContext::request`] / [`Page::request`] share the cookies of
//!   the browser context: cookies are read from the browser before every
//!   request (and every redirect hop) and `Set-Cookie` headers are stored
//!   back into it.
//!
//! Requests are sent from this process, not from the browser: their TLS and
//! HTTP/2 fingerprints are those of `rustls`/`hyper`, not Chrome's.

mod client_certificate;
mod cookie_store;
mod multipart;

use std::collections::HashMap;
use std::future::{Future, IntoFuture};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::browser_context::{BrowserContext, ContextInner};
use crate::cookies::{
    ClearCookiesFilter, Cookie, SameSite, SetCookie, TextMatch, domain_matches, filter_cookies,
    rewrite_cookies, url_hostname,
};
use crate::error::{Error, Result, ResultExt};
use crate::network::Header;
use crate::page::Page;
use crate::storage_state::{OriginState, StorageState, StorageStateSource};
use crate::url_match::construct_url_based_on_base_url;

pub use client_certificate::ClientCertificate;
pub use multipart::{FilePayload, MultipartValue};

use cookie_store::{CookieStore, parse_set_cookie};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

const REDIRECT_STATUS: [u16; 5] = [301, 302, 303, 307, 308];

/// Credentials for HTTP authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpCredentials {
    /// User name.
    pub username: String,
    /// Password.
    pub password: String,
    /// Only send the credentials to this origin (`scheme://host:port`).
    pub origin: Option<String>,
    /// When to send the `Authorization` header (default: on a `401` with a
    /// `Basic` challenge).
    pub send: Option<HttpCredentialsSend>,
}

impl HttpCredentials {
    /// Credentials for every origin, sent when challenged.
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            origin: None,
            send: None,
        }
    }

    /// Restrict the credentials to an origin.
    pub fn origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    /// When to send the credentials.
    pub fn send(mut self, send: HttpCredentialsSend) -> Self {
        self.send = Some(send);
        self
    }
}

/// When [`HttpCredentials`] are sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpCredentialsSend {
    /// With every request.
    Always,
    /// After a `401` response with a `Basic` challenge.
    Unauthorized,
}

/// Proxy of a standalone request context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySettings {
    /// `http://host:port`, `socks5://host:port`, ... (`host:port` = HTTP).
    pub server: String,
    /// Comma-separated hosts that bypass the proxy.
    pub bypass: Option<String>,
    /// Proxy user name.
    pub username: Option<String>,
    /// Proxy password.
    pub password: Option<String>,
}

impl ProxySettings {
    /// A proxy server without authentication.
    pub fn new(server: impl Into<String>) -> Self {
        Self {
            server: server.into(),
            bypass: None,
            username: None,
            password: None,
        }
    }
}

/// Defaults of every request of a context (`FetchRequestOptions`).
#[derive(Debug, Clone, Default)]
struct Defaults {
    base_url: Option<String>,
    user_agent: String,
    extra_http_headers: Vec<(String, String)>,
    http_credentials: Vec<HttpCredentials>,
    ignore_https_errors: bool,
    fail_on_status_code: bool,
    max_redirects: Option<u32>,
    proxy: Option<ProxySettings>,
    client_certificates: Vec<ClientCertificate>,
    timeout: u64,
}

/// `playwright.request`: creates standalone [`APIRequestContext`]s.
pub fn request() -> APIRequest {
    APIRequest
}

/// Factory of standalone request contexts (Playwright's `APIRequest`).
#[derive(Debug, Clone, Copy, Default)]
pub struct APIRequest;

impl APIRequest {
    /// Create a request context with its own cookie jar.
    ///
    /// ```no_run
    /// # async fn run() -> oxidewright::Result<()> {
    /// let request = oxidewright::request()
    ///     .new_context()
    ///     .base_url("https://api.example.com")
    ///     .extra_http_header("Authorization", "token 123")
    ///     .await?;
    /// let response = request.get("/users").param("page", 2).await?;
    /// let users: serde_json::Value = response.json().await?;
    /// # let _ = users; Ok(()) }
    /// ```
    pub fn new_context(&self) -> NewRequestContext {
        NewRequestContext {
            defaults: Defaults::default(),
            user_agent: None,
            timeout: None,
            storage_state: None,
        }
    }
}

/// Builder returned by [`APIRequest::new_context`]; `.await` it.
#[must_use = "builders do nothing unless awaited"]
pub struct NewRequestContext {
    defaults: Defaults,
    user_agent: Option<String>,
    timeout: Option<u64>,
    storage_state: Option<StorageStateSource>,
}

impl NewRequestContext {
    /// Base URL used to resolve relative request URLs.
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.defaults.base_url = Some(base_url.into());
        self
    }

    /// A header sent with every request.
    pub fn extra_http_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.defaults
            .extra_http_headers
            .push((name.into(), value.into()));
        self
    }

    /// Headers sent with every request.
    pub fn extra_http_headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.defaults
            .extra_http_headers
            .extend(headers.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// HTTP authentication credentials (can be repeated for several
    /// origins; the first matching entry is used).
    pub fn http_credentials(mut self, credentials: HttpCredentials) -> Self {
        self.defaults.http_credentials.push(credentials);
        self
    }

    /// Ignore HTTPS certificate errors.
    pub fn ignore_https_errors(mut self, ignore: bool) -> Self {
        self.defaults.ignore_https_errors = ignore;
        self
    }

    /// `User-Agent` of the requests.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    /// Default timeout of requests in ms (default 30000, `0` = none).
    pub fn timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout = Some(timeout_ms);
        self
    }

    /// Fail on non-2xx/3xx responses by default.
    pub fn fail_on_status_code(mut self, fail: bool) -> Self {
        self.defaults.fail_on_status_code = fail;
        self
    }

    /// Maximum number of redirects followed by default (20; `0` = none).
    pub fn max_redirects(mut self, max: u32) -> Self {
        self.defaults.max_redirects = Some(max);
        self
    }

    /// Send requests through a proxy.
    pub fn proxy(mut self, proxy: ProxySettings) -> Self {
        self.defaults.proxy = Some(proxy);
        self
    }

    /// TLS client certificates, each presented to its own origin.
    pub fn client_certificates(
        mut self,
        certificates: impl IntoIterator<Item = ClientCertificate>,
    ) -> Self {
        self.defaults.client_certificates = certificates.into_iter().collect();
        self
    }

    /// Initial cookies (and origins reported by `storage_state`).
    pub fn storage_state(mut self, state: impl Into<StorageStateSource>) -> Self {
        self.storage_state = Some(state.into());
        self
    }
}

impl IntoFuture for NewRequestContext {
    type Output = Result<APIRequestContext>;
    type IntoFuture = BoxFuture<Result<APIRequestContext>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            async move {
                let mut defaults = self.defaults;
                client_certificate::validate(&defaults.client_certificates)?;
                defaults.user_agent = self.user_agent.unwrap_or_else(default_user_agent);
                defaults.timeout = self.timeout.unwrap_or(crate::DEFAULT_TIMEOUT);
                let mut store = CookieStore::default();
                let mut origins = Vec::new();
                if let Some(source) = &self.storage_state {
                    let state = source.load().await?;
                    store.add_cookies(state.cookies);
                    origins = state.origins;
                }
                Ok(APIRequestContext::new(Kind::Global {
                    defaults,
                    store: Mutex::new(store),
                    origins,
                }))
            }
            .await
            .api("apiRequest.newContext")
        })
    }
}

/// A Chrome-like user agent for standalone contexts.
fn default_user_agent() -> String {
    let platform = match std::env::consts::OS {
        "windows" => "Windows NT 10.0; Win64; x64",
        "macos" => "Macintosh; Intel Mac OS X 10_15_7",
        _ => "X11; Linux x86_64",
    };
    format!(
        "Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36"
    )
}

// One per request context: the size difference does not matter.
#[allow(clippy::large_enum_variant)]
enum Kind {
    Global {
        defaults: Defaults,
        store: Mutex<CookieStore>,
        origins: Vec<OriginState>,
    },
    Browser {
        context: Weak<ContextInner>,
        user_agent: tokio::sync::OnceCell<String>,
    },
}

struct Inner {
    kind: Kind,
    /// HTTP clients by (ignore HTTPS errors, client certificate).
    clients: Mutex<HashMap<(bool, Option<usize>), reqwest::Client>>,
    responses: Mutex<HashMap<u64, Arc<Vec<u8>>>>,
    next_uid: AtomicU64,
    disposed: AtomicBool,
    close_reason: Mutex<Option<String>>,
    dispose_signal: tokio::sync::watch::Sender<bool>,
}

/// Sends HTTP requests and keeps cookies (Playwright's
/// `APIRequestContext`). Cheap to clone.
#[derive(Clone)]
pub struct APIRequestContext {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for APIRequestContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.inner.kind {
            Kind::Global { .. } => "global",
            Kind::Browser { .. } => "browser-context",
        };
        f.debug_struct("APIRequestContext")
            .field("kind", &kind)
            .finish()
    }
}

impl BrowserContext {
    /// The [`APIRequestContext`] of this context: requests share the
    /// context's cookies.
    pub fn request(&self) -> APIRequestContext {
        self.inner
            .emu
            .request
            .get_or_init(|| APIRequestContext::for_context(Arc::downgrade(&self.inner)))
            .clone()
    }
}

impl Page {
    /// The [`APIRequestContext`] of the page's context.
    pub fn request(&self) -> APIRequestContext {
        match self.context() {
            Some(context) => context.request(),
            None => APIRequestContext::for_context(self.inner.context.clone()),
        }
    }
}

impl APIRequestContext {
    fn new(kind: Kind) -> Self {
        Self {
            inner: Arc::new(Inner {
                kind,
                clients: Mutex::new(HashMap::new()),
                responses: Mutex::new(HashMap::new()),
                next_uid: AtomicU64::new(1),
                disposed: AtomicBool::new(false),
                close_reason: Mutex::new(None),
                dispose_signal: tokio::sync::watch::channel(false).0,
            }),
        }
    }

    fn for_context(context: Weak<ContextInner>) -> Self {
        Self::new(Kind::Browser {
            context,
            user_agent: tokio::sync::OnceCell::new(),
        })
    }

    fn browser_context(&self) -> Result<Option<BrowserContext>> {
        match &self.inner.kind {
            Kind::Global { .. } => Ok(None),
            Kind::Browser { context, .. } => match context.upgrade() {
                Some(inner) => Ok(Some(BrowserContext { inner })),
                None => Err(Error::target_closed()),
            },
        }
    }

    /// Error when the context can no longer send requests.
    fn closed_error(&self) -> Option<Error> {
        if let Some(reason) = self.inner.close_reason.lock().unwrap().clone() {
            return Some(Error::target_closed_with(reason));
        }
        if self.inner.disposed.load(Ordering::SeqCst) {
            return Some(Error::target_closed());
        }
        match &self.inner.kind {
            Kind::Global { .. } => None,
            Kind::Browser { context, .. } => match context.upgrade() {
                Some(inner) => {
                    let context = BrowserContext { inner };
                    context.is_closed().then(|| match context.close_reason() {
                        Some(reason) => Error::target_closed_with(reason),
                        None => Error::target_closed(),
                    })
                }
                None => Some(Error::target_closed()),
            },
        }
    }

    fn responses_disposed(&self) -> bool {
        self.inner.disposed.load(Ordering::SeqCst) || self.closed_error().is_some()
    }

    async fn defaults(&self) -> Result<Defaults> {
        match &self.inner.kind {
            Kind::Global { defaults, .. } => Ok(defaults.clone()),
            Kind::Browser {
                context,
                user_agent,
            } => {
                let inner = context.upgrade().ok_or_else(Error::target_closed)?;
                let context = BrowserContext { inner };
                let options = &context.inner.options;
                let user_agent = match &options.user_agent {
                    Some(ua) => ua.clone(),
                    None => user_agent
                        .get_or_try_init(|| async {
                            let version = context
                                .browser_command("Browser.getVersion", Value::Null)
                                .await?;
                            Ok::<_, Error>(
                                version
                                    .get("userAgent")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            )
                        })
                        .await?
                        .clone(),
                };
                Ok(Defaults {
                    base_url: options.base_url.clone(),
                    user_agent,
                    extra_http_headers: context
                        .inner
                        .ext
                        .extra_headers
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|h| (h.name.clone(), h.value.clone()))
                        .collect(),
                    http_credentials: context
                        .inner
                        .ext
                        .http_credentials
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|c| HttpCredentials {
                            username: c.username.clone(),
                            password: c.password.clone(),
                            origin: c.origin.clone(),
                            send: None,
                        })
                        .collect(),
                    ignore_https_errors: options.ignore_https_errors,
                    fail_on_status_code: false,
                    max_redirects: None,
                    proxy: None,
                    // Browser contexts do not take client certificates yet.
                    client_certificates: Vec::new(),
                    timeout: context.inner.timeouts.timeout(None),
                })
            }
        }
    }

    async fn client(
        &self,
        ignore_https_errors: bool,
        proxy: Option<&ProxySettings>,
        certificate: Option<(usize, &ClientCertificate)>,
    ) -> Result<reqwest::Client> {
        let key = (ignore_https_errors, certificate.map(|(i, _)| i));
        if let Some(client) = self.inner.clients.lock().unwrap().get(&key) {
            return Ok(client.clone());
        }
        let identity = match certificate {
            Some((_, certificate)) => {
                let pem = certificate.identity_pem().await?;
                Some(reqwest::Identity::from_pem(&pem).map_err(|e| {
                    Error::invalid_argument(format!("Invalid client certificate: {e}"))
                })?)
            }
            None => None,
        };
        ensure_crypto_provider();
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .tls_info(true)
            .tls_danger_accept_invalid_certs(ignore_https_errors);
        if let Some(identity) = identity {
            builder = builder.identity(identity);
        }
        builder = match proxy {
            Some(proxy) => {
                let server = if proxy.server.contains("://") {
                    proxy.server.clone()
                } else {
                    format!("http://{}", proxy.server)
                };
                let mut p = reqwest::Proxy::all(&server)
                    .map_err(|e| Error::invalid_argument(format!("Invalid proxy: {e}")))?;
                if let Some(username) = &proxy.username {
                    p = p.basic_auth(username, proxy.password.as_deref().unwrap_or_default());
                }
                if let Some(bypass) = &proxy.bypass {
                    p = p.no_proxy(reqwest::NoProxy::from_string(bypass));
                }
                builder.proxy(p)
            }
            None => builder.no_proxy(),
        };
        let client = builder.build().map_err(|e| {
            Error::invalid_argument(format!("Failed to create the HTTP client: {e}"))
        })?;
        self.inner
            .clients
            .lock()
            .unwrap()
            .insert(key, client.clone());
        Ok(client)
    }

    async fn cookies_for(&self, urls: &[String]) -> Result<Vec<Cookie>> {
        match &self.inner.kind {
            Kind::Global { store, .. } => {
                let all = store.lock().unwrap().all_cookies();
                filter_cookies(all, urls)
            }
            Kind::Browser { .. } => {
                let context = self.browser_context()?.ok_or_else(Error::target_closed)?;
                context.get_cookies(urls).await
            }
        }
    }

    async fn store_cookies(&self, cookies: Vec<SetCookie>) -> Result<()> {
        match &self.inner.kind {
            Kind::Global { store, .. } => {
                let cookies = rewrite_cookies(cookies)?;
                store
                    .lock()
                    .unwrap()
                    .add_cookies(cookies.into_iter().map(|c| Cookie {
                        name: c.name,
                        value: c.value,
                        domain: c.domain.unwrap_or_default(),
                        path: c.path.unwrap_or_default(),
                        expires: c.expires.unwrap_or(-1.0),
                        http_only: c.http_only.unwrap_or(false),
                        secure: c.secure.unwrap_or(false),
                        same_site: c.same_site.unwrap_or(SameSite::Lax),
                        partition_key: c.partition_key,
                        cr_has_cross_site_ancestor: c.cr_has_cross_site_ancestor,
                    }));
                Ok(())
            }
            Kind::Browser { .. } => {
                let context = self.browser_context()?.ok_or_else(Error::target_closed)?;
                context.set_cookies(cookies).await
            }
        }
    }

    /// A `GET` request.
    pub fn get(&self, url: impl Into<String>) -> Fetch {
        Fetch::new(
            self.clone(),
            url.into(),
            Some("GET"),
            "apiRequestContext.get",
        )
    }

    /// A `POST` request.
    pub fn post(&self, url: impl Into<String>) -> Fetch {
        Fetch::new(
            self.clone(),
            url.into(),
            Some("POST"),
            "apiRequestContext.post",
        )
    }

    /// A `PUT` request.
    pub fn put(&self, url: impl Into<String>) -> Fetch {
        Fetch::new(
            self.clone(),
            url.into(),
            Some("PUT"),
            "apiRequestContext.put",
        )
    }

    /// A `PATCH` request.
    pub fn patch(&self, url: impl Into<String>) -> Fetch {
        Fetch::new(
            self.clone(),
            url.into(),
            Some("PATCH"),
            "apiRequestContext.patch",
        )
    }

    /// A `DELETE` request.
    pub fn delete(&self, url: impl Into<String>) -> Fetch {
        Fetch::new(
            self.clone(),
            url.into(),
            Some("DELETE"),
            "apiRequestContext.delete",
        )
    }

    /// A `HEAD` request.
    pub fn head(&self, url: impl Into<String>) -> Fetch {
        Fetch::new(
            self.clone(),
            url.into(),
            Some("HEAD"),
            "apiRequestContext.head",
        )
    }

    /// A request (`GET` unless [`Fetch::method`] says otherwise).
    pub fn fetch(&self, url: impl Into<String>) -> Fetch {
        Fetch::new(self.clone(), url.into(), None, "apiRequestContext.fetch")
    }

    /// Cookies of the context, all of them or the ones sent to the URLs.
    pub fn cookies(&self) -> RequestCookies {
        RequestCookies {
            context: self.clone(),
            urls: Vec::new(),
        }
    }

    /// Add cookies; each needs `url` or `domain` + `path`.
    pub async fn add_cookies<I, C>(&self, cookies: I) -> Result<()>
    where
        I: IntoIterator<Item = C>,
        C: Into<SetCookie>,
    {
        self.store_cookies(cookies.into_iter().map(Into::into).collect())
            .await
            .api("apiRequestContext.addCookies")
    }

    /// Remove cookies, all of them or the ones matching the filters.
    pub fn clear_cookies(&self) -> RequestClearCookies {
        RequestClearCookies {
            context: self.clone(),
            filter: ClearCookiesFilter::default(),
        }
    }

    /// Cookies and storage of the context (for a browser context's
    /// request context, the browser context's storage state).
    pub fn storage_state(&self) -> RequestStorageState {
        RequestStorageState {
            context: self.clone(),
            path: None,
            indexed_db: false,
            opfs: false,
        }
    }

    /// Release stored response bodies; standalone contexts can no longer
    /// send requests afterwards.
    pub fn dispose(&self) -> DisposeRequestContext {
        DisposeRequestContext {
            context: self.clone(),
            reason: None,
        }
    }
}

/// Builder returned by [`APIRequestContext::dispose`]; `.await` it.
#[must_use = "builders do nothing unless awaited"]
pub struct DisposeRequestContext {
    context: APIRequestContext,
    reason: Option<String>,
}

impl DisposeRequestContext {
    /// Reason reported by operations interrupted by the disposal.
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

impl IntoFuture for DisposeRequestContext {
    type Output = Result<()>;
    type IntoFuture = BoxFuture<Result<()>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let inner = &self.context.inner;
            if let Some(reason) = self.reason {
                *inner.close_reason.lock().unwrap() = Some(reason);
            }
            inner.responses.lock().unwrap().clear();
            inner.disposed.store(true, Ordering::SeqCst);
            inner.dispose_signal.send_replace(true);
            Ok(())
        })
    }
}

/// Builder returned by [`APIRequestContext::cookies`]; `.await` it.
#[must_use = "builders do nothing unless awaited"]
pub struct RequestCookies {
    context: APIRequestContext,
    urls: Vec<String>,
}

impl RequestCookies {
    /// Only cookies sent to this URL (can be repeated).
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.urls.push(url.into());
        self
    }
}

impl IntoFuture for RequestCookies {
    type Output = Result<Vec<Cookie>>;
    type IntoFuture = BoxFuture<Result<Vec<Cookie>>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            self.context
                .cookies_for(&self.urls)
                .await
                .api("apiRequestContext.cookies")
        })
    }
}

/// Builder returned by [`APIRequestContext::clear_cookies`]; `.await` it.
#[must_use = "builders do nothing unless awaited"]
pub struct RequestClearCookies {
    context: APIRequestContext,
    filter: ClearCookiesFilter,
}

impl RequestClearCookies {
    /// Only cookies with this name.
    pub fn name(mut self, name: impl Into<TextMatch>) -> Self {
        self.filter.name = Some(name.into());
        self
    }

    /// Only cookies with this domain.
    pub fn domain(mut self, domain: impl Into<TextMatch>) -> Self {
        self.filter.domain = Some(domain.into());
        self
    }

    /// Only cookies with this path.
    pub fn path(mut self, path: impl Into<TextMatch>) -> Self {
        self.filter.path = Some(path.into());
        self
    }
}

impl IntoFuture for RequestClearCookies {
    type Output = Result<()>;
    type IntoFuture = BoxFuture<Result<()>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            async {
                match &self.context.inner.kind {
                    Kind::Global { store, .. } => {
                        let filter = self.filter;
                        store.lock().unwrap().remove_cookies(|c| filter.matches(c));
                        Ok(())
                    }
                    Kind::Browser { .. } => {
                        let context = self
                            .context
                            .browser_context()?
                            .ok_or_else(Error::target_closed)?;
                        context.clear_cookies_filtered(&self.filter).await
                    }
                }
            }
            .await
            .api("apiRequestContext.clearCookies")
        })
    }
}

/// Builder returned by [`APIRequestContext::storage_state`]; `.await` it.
#[must_use = "builders do nothing unless awaited"]
pub struct RequestStorageState {
    context: APIRequestContext,
    path: Option<PathBuf>,
    indexed_db: bool,
    opfs: bool,
}

impl RequestStorageState {
    /// Also write the state as JSON to this file.
    pub fn path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Include IndexedDB databases.
    pub fn indexed_db(mut self, indexed_db: bool) -> Self {
        self.indexed_db = indexed_db;
        self
    }

    /// Include the origin private file system (OPFS) entries.
    pub fn opfs(mut self, opfs: bool) -> Self {
        self.opfs = opfs;
        self
    }
}

impl IntoFuture for RequestStorageState {
    type Output = Result<StorageState>;
    type IntoFuture = BoxFuture<Result<StorageState>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            async {
                let state = match &self.context.inner.kind {
                    Kind::Global { store, origins, .. } => StorageState {
                        cookies: store.lock().unwrap().all_cookies(),
                        origins: origins
                            .iter()
                            .map(|o| OriginState {
                                indexed_db: Some(if self.indexed_db {
                                    o.indexed_db.clone().unwrap_or_default()
                                } else {
                                    Vec::new()
                                }),
                                opfs: if self.opfs { o.opfs.clone() } else { None },
                                ..o.clone()
                            })
                            .collect(),
                    },
                    Kind::Browser { .. } => {
                        let context = self
                            .context
                            .browser_context()?
                            .ok_or_else(Error::target_closed)?;
                        context
                            .collect_storage_state(self.indexed_db, self.opfs)
                            .await?
                    }
                };
                if let Some(path) = &self.path {
                    crate::storage_state::write_json(path, &state).await?;
                }
                Ok(state)
            }
            .await
            .api("apiRequestContext.storageState")
        })
    }
}

// ----------------------------------------------------------------------
// Requests
// ----------------------------------------------------------------------

/// The `data` of a request.
#[derive(Debug, Clone, PartialEq)]
pub enum RequestData {
    /// Text; sent as JSON when the `content-type` header is JSON.
    Text(String),
    /// Raw bytes (`application/octet-stream` unless set).
    Bytes(Vec<u8>),
    /// A JSON value (`application/json`).
    Json(Value),
}

impl From<&str> for RequestData {
    fn from(v: &str) -> Self {
        RequestData::Text(v.to_string())
    }
}

impl From<String> for RequestData {
    fn from(v: String) -> Self {
        RequestData::Text(v)
    }
}

impl From<Vec<u8>> for RequestData {
    fn from(v: Vec<u8>) -> Self {
        RequestData::Bytes(v)
    }
}

impl From<&[u8]> for RequestData {
    fn from(v: &[u8]) -> Self {
        RequestData::Bytes(v.to_vec())
    }
}

impl From<Value> for RequestData {
    fn from(v: Value) -> Self {
        RequestData::Json(v)
    }
}

enum MultipartInput {
    Value(MultipartValue),
    Path(PathBuf),
}

/// A request being built; returned by `get`/`post`/.../`fetch`, `.await` it
/// for the [`APIResponse`].
#[must_use = "builders do nothing unless awaited"]
pub struct Fetch {
    context: APIRequestContext,
    api: &'static str,
    url: String,
    method: Option<String>,
    params: Vec<(String, String)>,
    encoded_params: Option<String>,
    headers: Vec<(String, String)>,
    data: Option<Result<RequestData>>,
    form: Option<Vec<(String, String)>>,
    multipart: Option<Vec<(String, MultipartInput)>>,
    fail_on_status_code: Option<bool>,
    ignore_https_errors: Option<bool>,
    max_redirects: Option<u32>,
    max_retries: Option<u32>,
    timeout: Option<u64>,
}

impl Fetch {
    fn new(
        context: APIRequestContext,
        url: String,
        method: Option<&str>,
        api: &'static str,
    ) -> Self {
        Self {
            context,
            api,
            url,
            method: method.map(str::to_string),
            params: Vec::new(),
            encoded_params: None,
            headers: Vec::new(),
            data: None,
            form: None,
            multipart: None,
            fail_on_status_code: None,
            ignore_https_errors: None,
            max_redirects: None,
            max_retries: None,
            timeout: None,
        }
    }

    /// HTTP method (`fetch` only; default `GET`).
    pub fn method(mut self, method: impl Into<String>) -> Self {
        self.method = Some(method.into());
        self
    }

    /// Append a query parameter.
    pub fn param(mut self, name: impl Into<String>, value: impl ToString) -> Self {
        self.params.push((name.into(), value.to_string()));
        self
    }

    /// Append query parameters.
    pub fn params<I, K, V>(mut self, params: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: ToString,
    {
        self.params
            .extend(params.into_iter().map(|(k, v)| (k.into(), v.to_string())));
        self
    }

    /// Append an already encoded query string (`"a=1&b=2"`).
    pub fn encoded_params(mut self, query: impl Into<String>) -> Self {
        self.encoded_params = Some(query.into());
        self
    }

    /// Set a request header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Set request headers.
    pub fn headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.headers
            .extend(headers.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Request body: text, bytes or a JSON value.
    pub fn data(mut self, data: impl Into<RequestData>) -> Self {
        self.data = Some(Ok(data.into()));
        self
    }

    /// Request body: any serializable value, sent as JSON.
    pub fn json<T: Serialize + ?Sized>(mut self, value: &T) -> Self {
        self.data = Some(
            serde_json::to_value(value)
                .map(RequestData::Json)
                .map_err(Error::from),
        );
        self
    }

    /// Add a `application/x-www-form-urlencoded` field.
    pub fn form_field(mut self, name: impl Into<String>, value: impl ToString) -> Self {
        self.form
            .get_or_insert_with(Vec::new)
            .push((name.into(), value.to_string()));
        self
    }

    /// Add `application/x-www-form-urlencoded` fields.
    pub fn form<I, K, V>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: ToString,
    {
        self.form
            .get_or_insert_with(Vec::new)
            .extend(fields.into_iter().map(|(k, v)| (k.into(), v.to_string())));
        self
    }

    /// Add a `multipart/form-data` field (text or [`FilePayload`]).
    pub fn multipart_field(
        mut self,
        name: impl Into<String>,
        value: impl Into<MultipartValue>,
    ) -> Self {
        self.multipart
            .get_or_insert_with(Vec::new)
            .push((name.into(), MultipartInput::Value(value.into())));
        self
    }

    /// Add a `multipart/form-data` file field read from disk.
    pub fn multipart_file(mut self, name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        self.multipart
            .get_or_insert_with(Vec::new)
            .push((name.into(), MultipartInput::Path(path.into())));
        self
    }

    /// Fail on non-2xx/3xx responses.
    pub fn fail_on_status_code(mut self, fail: bool) -> Self {
        self.fail_on_status_code = Some(fail);
        self
    }

    /// Ignore HTTPS certificate errors.
    pub fn ignore_https_errors(mut self, ignore: bool) -> Self {
        self.ignore_https_errors = Some(ignore);
        self
    }

    /// Maximum number of redirects (default 20, `0` = do not follow).
    pub fn max_redirects(mut self, max: u32) -> Self {
        self.max_redirects = Some(max);
        self
    }

    /// Retries on connection resets (default 0).
    pub fn max_retries(mut self, max: u32) -> Self {
        self.max_retries = Some(max);
        self
    }

    /// Timeout in ms (`0` = none).
    pub fn timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout = Some(timeout_ms);
        self
    }
}

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// `setHeader`: replace case-insensitively (unless `keep_existing`).
pub(crate) fn set_header(
    headers: &mut Vec<(String, String)>,
    name: &str,
    value: &str,
    keep_existing: bool,
) {
    match headers
        .iter_mut()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
    {
        Some(existing) => {
            if !keep_existing {
                existing.1 = value.to_string();
            }
        }
        None => headers.push((name.to_string(), value.to_string())),
    }
}

fn remove_header(headers: &mut Vec<(String, String)>, name: &str) {
    headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
}

fn basic_auth(credentials: &HttpCredentials) -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", credentials.username, credentials.password))
    )
}

/// `findMatchingHttpCredentials`.
fn matching_credentials<'a>(
    credentials: &'a [HttpCredentials],
    url: &url::Url,
) -> Option<&'a HttpCredentials> {
    let origin = url.origin().ascii_serialization().to_lowercase();
    credentials.iter().find(|c| match &c.origin {
        None => true,
        Some(o) => o.to_lowercase() == origin,
    })
}

fn is_json_content_type(headers: &[(String, String)]) -> bool {
    find_header(headers, "content-type").is_some_and(|v| {
        v.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("application/json")
    })
}

/// Text of an error and its sources, Node-like for common socket errors.
fn describe_error(err: &reqwest::Error, url: &url::Url) -> (String, bool) {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    let mut reset = false;
    let mut deepest = err.to_string();
    while let Some(e) = source {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            let host = url_hostname(url);
            let port = url.port_or_known_default().unwrap_or_default();
            match io.kind() {
                std::io::ErrorKind::ConnectionRefused => {
                    return (format!("connect ECONNREFUSED {host}:{port}"), false);
                }
                std::io::ErrorKind::ConnectionReset => {
                    reset = true;
                    deepest = "read ECONNRESET".to_string();
                }
                _ => {}
            }
        }
        let text = e.to_string();
        if !text.is_empty() && !reset {
            deepest = text;
        }
        source = e.source();
    }
    (deepest, reset)
}

struct RawResponse {
    url: String,
    status: u16,
    status_text: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// Playwright's fetch log (`→ GET url`, headers, `← 200 OK`, headers).
    log: Vec<String>,
    server_addr: Option<crate::network::RemoteAddr>,
    security_details: Option<crate::network::SecurityDetails>,
    timing: crate::network::ResourceTiming,
}

struct SendError {
    error: Error,
    reset: bool,
}

impl From<Error> for SendError {
    fn from(error: Error) -> Self {
        SendError {
            error,
            reset: false,
        }
    }
}

impl Fetch {
    async fn prepare_body(
        &mut self,
        headers: &mut Vec<(String, String)>,
    ) -> Result<Option<Vec<u8>>> {
        let given = (self.data.is_some() as u8)
            + (self.form.is_some() as u8)
            + (self.multipart.is_some() as u8);
        if given > 1 {
            return Err(Error::invalid_argument(
                "Only one of 'data', 'form' or 'multipart' can be specified",
            ));
        }
        if let Some(data) = self.data.take() {
            let (bytes, json) = match data? {
                RequestData::Text(text) => {
                    if is_json_content_type(&self.headers) {
                        let json = if serde_json::from_str::<Value>(&text).is_ok() {
                            text
                        } else {
                            serde_json::to_string(&text)?
                        };
                        (json.into_bytes(), true)
                    } else {
                        (text.into_bytes(), false)
                    }
                }
                RequestData::Bytes(bytes) => (bytes, false),
                RequestData::Json(value) => (serde_json::to_string(&value)?.into_bytes(), true),
            };
            let content_type = if json {
                "application/json"
            } else {
                "application/octet-stream"
            };
            set_header(headers, "content-type", content_type, true);
            return Ok(Some(bytes));
        }
        if let Some(form) = self.form.take() {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for (name, value) in &form {
                serializer.append_pair(name, value);
            }
            set_header(
                headers,
                "content-type",
                "application/x-www-form-urlencoded",
                true,
            );
            return Ok(Some(serializer.finish().into_bytes()));
        }
        if let Some(fields) = self.multipart.take() {
            let mut resolved = Vec::with_capacity(fields.len());
            for (name, input) in fields {
                let value = match input {
                    MultipartInput::Value(v) => v,
                    MultipartInput::Path(path) => {
                        let buffer = tokio::fs::read(&path).await.map_err(|e| {
                            Error::invalid_argument(format!("{}: {e}", path.display()))
                        })?;
                        MultipartValue::File(FilePayload {
                            name: path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default(),
                            mime_type: None,
                            buffer,
                        })
                    }
                };
                resolved.push((name, value));
            }
            let (content_type, body) = multipart::encode(&resolved);
            set_header(headers, "content-type", &content_type, true);
            return Ok(Some(body));
        }
        Ok(None)
    }

    async fn run(mut self) -> Result<APIResponse> {
        if let Some(err) = self.context.closed_error() {
            return Err(err);
        }
        let defaults = self.context.defaults().await?;
        let timeout = self.timeout.unwrap_or(defaults.timeout);
        let mut headers: Vec<(String, String)> = vec![
            ("user-agent".into(), defaults.user_agent.clone()),
            ("accept".into(), "*/*".into()),
            ("accept-encoding".into(), "gzip,deflate,br".into()),
        ];
        for (name, value) in &defaults.extra_http_headers {
            set_header(&mut headers, name, value, false);
        }
        for (name, value) in &self.headers {
            set_header(&mut headers, name, value, false);
        }
        let resolved = construct_url_based_on_base_url(defaults.base_url.as_deref(), &self.url);
        let mut url = url::Url::parse(&resolved)
            .map_err(|_| Error::invalid_argument(format!("Invalid URL: {resolved}")))?;
        if let Some(encoded) = &self.encoded_params {
            let encoded = encoded.trim_start_matches('?');
            if !encoded.is_empty() {
                let query = match url.query() {
                    Some(q) if !q.is_empty() => format!("{q}&{encoded}"),
                    _ => encoded.to_string(),
                };
                url.set_query(Some(&query));
            }
        } else if !self.params.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (name, value) in &self.params {
                pairs.append_pair(name, value);
            }
        }
        if let Some(credentials) = matching_credentials(&defaults.http_credentials, &url) {
            if credentials.send == Some(HttpCredentialsSend::Always) {
                set_header(
                    &mut headers,
                    "authorization",
                    &basic_auth(credentials),
                    false,
                );
            }
        }
        let method = self
            .method
            .clone()
            .unwrap_or_else(|| "GET".to_string())
            .to_uppercase();
        let max_redirects = self.max_redirects.or(defaults.max_redirects).unwrap_or(20);
        let follow: i64 = if max_redirects == 0 {
            -1
        } else {
            max_redirects as i64
        };
        let body = self.prepare_body(&mut headers).await?;
        let ignore_https_errors =
            self.ignore_https_errors.unwrap_or(false) || defaults.ignore_https_errors;
        // The certificate of the request's origin (a redirect to another
        // origin keeps this client).
        let certificate = defaults
            .client_certificates
            .iter()
            .enumerate()
            .find(|(_, c)| c.matches(url.as_str()));
        let client = self
            .context
            .client(ignore_https_errors, defaults.proxy.as_ref(), certificate)
            .await?;
        let max_retries = self.max_retries.unwrap_or(0);

        let context = self.context.clone();
        let send = async {
            let mut backoff = 250;
            let mut attempt = 0;
            loop {
                match send_request(
                    &context,
                    &client,
                    &defaults,
                    url.clone(),
                    method.clone(),
                    headers.clone(),
                    body.clone(),
                    follow,
                )
                .await
                {
                    Ok(response) => return Ok(response),
                    Err(err) => {
                        if max_retries == 0 {
                            return Err(err.error);
                        }
                        if attempt == max_retries {
                            return Err(Error::navigation(format!(
                                "Failed after {} attempt(s): {}",
                                attempt + 1,
                                err.error.message()
                            )));
                        }
                        if !err.reset {
                            return Err(err.error);
                        }
                        tokio::time::sleep(Duration::from_millis(backoff)).await;
                        backoff *= 2;
                        attempt += 1;
                    }
                }
            }
        };
        let mut disposed = self.context.inner.dispose_signal.subscribe();
        let context_closed = async {
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if let Kind::Browser { .. } = context.inner.kind {
                    if let Some(err) = context.closed_error() {
                        if !context.inner.disposed.load(Ordering::SeqCst) {
                            return err;
                        }
                    }
                }
            }
        };
        let send = async {
            tokio::select! {
                result = send => result,
                err = context_closed => Err(err),
                _ = disposed.wait_for(|d| *d) => Err(match context.inner.close_reason.lock().unwrap().clone() {
                    Some(reason) => Error::target_closed_with(reason),
                    None => Error::target_closed_with("Request context disposed."),
                }),
            }
        };
        let raw = if timeout == 0 {
            send.await?
        } else {
            match tokio::time::timeout(Duration::from_millis(timeout), send).await {
                Ok(result) => result?,
                Err(_) => return Err(Error::timeout_exceeded(timeout)),
            }
        };
        let fail = self
            .fail_on_status_code
            .unwrap_or(defaults.fail_on_status_code);
        if fail && (raw.status < 200 || raw.status >= 400) {
            let mut text = String::new();
            if !raw.body.is_empty() {
                let mut body = String::from_utf8_lossy(&raw.body).to_string();
                if body.chars().count() > 1000 {
                    body = body.chars().take(997).collect::<String>() + "...";
                }
                text = format!("\nResponse text:\n{body}");
            }
            return Err(Error::navigation(format!(
                "{} {}{text}",
                raw.status, raw.status_text
            )));
        }
        let uid = self.context.inner.next_uid.fetch_add(1, Ordering::SeqCst);
        self.context
            .inner
            .responses
            .lock()
            .unwrap()
            .insert(uid, Arc::new(raw.body));
        Ok(APIResponse {
            body: BodySource::Stored {
                context: self.context.clone(),
                uid,
            },
            url: raw.url,
            status: raw.status,
            status_text: raw.status_text,
            headers: raw.headers,
            log: Arc::new(raw.log),
            server_addr: raw.server_addr,
            security_details: raw.security_details,
            timing: raw.timing,
        })
    }
}

impl IntoFuture for Fetch {
    type Output = Result<APIResponse>;
    type IntoFuture = BoxFuture<Result<APIResponse>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let api = self.api;
            self.run().await.map_err(|e| e.with_api(api))
        })
    }
}

/// `_parseSetCookieHeader` + `addCookies` for one response.
async fn store_response_cookies(context: &APIRequestContext, url: &url::Url, values: &[String]) {
    let hostname = url_hostname(url);
    let path = url.path();
    let default_path = {
        let segments: Vec<&str> = path[1.min(path.len())..].split('/').collect();
        format!(
            "/{}",
            segments[..segments.len().saturating_sub(1)].join("/")
        )
    };
    let mut cookies = Vec::new();
    for header in values {
        let Some(mut cookie) = parse_set_cookie(header) else {
            continue;
        };
        if cookie.domain.is_empty() {
            cookie.domain = hostname.clone();
        } else if !(cookie.domain.starts_with('.') || !cookie.domain.contains('.')) {
            continue;
        }
        if !domain_matches(&hostname, &cookie.domain) {
            continue;
        }
        if cookie.path.is_empty() || !cookie.path.starts_with('/') {
            cookie.path = default_path.clone();
        }
        cookies.push(SetCookie::from(cookie));
    }
    if cookies.is_empty() {
        return;
    }
    if context.store_cookies(cookies.clone()).await.is_err() {
        for cookie in cookies {
            let _ = context.store_cookies(vec![cookie]).await;
        }
    }
}

fn header_text(value: &reqwest::header::HeaderValue) -> String {
    match value.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => {
            let bytes = value.as_bytes();
            match std::str::from_utf8(bytes) {
                Ok(s) => s.to_string(),
                Err(_) => bytes.iter().map(|b| *b as char).collect(),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_request(
    context: &APIRequestContext,
    client: &reqwest::Client,
    defaults: &Defaults,
    mut url: url::Url,
    mut method: String,
    mut headers: Vec<(String, String)>,
    mut body: Option<Vec<u8>>,
    mut redirects_left: i64,
) -> std::result::Result<RawResponse, SendError> {
    let mut log: Vec<String> = Vec::new();
    loop {
        // `_updateRequestCookieHeader`
        if find_header(&headers, "cookie").is_none() {
            let cookies = context.cookies_for(&[url.to_string()]).await?;
            if !cookies.is_empty() {
                let value = cookies
                    .iter()
                    .map(|c| cookie_store::serialize_cookie_name_value(&c.name, &c.value))
                    .collect::<Vec<_>>()
                    .join("; ");
                set_header(&mut headers, "cookie", &value, false);
            }
        }
        if let Some(err) = context.closed_error() {
            return Err(err.into());
        }
        let http_method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| Error::invalid_argument(format!("Invalid method: {method}")))?;
        let mut request = client.request(http_method, url.clone());
        log.push(format!("→ {method} {url}"));
        for (name, value) in &headers {
            log.push(format!("  {name}: {value}"));
        }
        for (name, value) in &headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(body) = &body {
            request = request.body(body.clone());
        }
        let start_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or_default();
        let started = std::time::Instant::now();
        let response = match request.send().await {
            Ok(r) => r,
            Err(err) => {
                let (message, reset) = describe_error(&err, &url);
                return Err(SendError {
                    error: Error::navigation(message),
                    reset,
                });
            }
        };
        let response_start = started.elapsed().as_secs_f64() * 1000.0;
        let server_addr = response.remote_addr().map(|a| crate::network::RemoteAddr {
            ip_address: a.ip().to_string(),
            port: a.port(),
        });
        let security_details = response
            .extensions()
            .get::<reqwest::tls::TlsInfo>()
            .and_then(|info| info.peer_certificate())
            .map(certificate_details);
        let status = response.status().as_u16();
        let status_text = response
            .status()
            .canonical_reason()
            .unwrap_or_default()
            .to_string();
        let response_headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_string(), header_text(v)))
            .collect();
        log.push(format!("← {status} {status_text}"));
        for (name, value) in &response_headers {
            log.push(format!("  {name}: {value}"));
        }
        let set_cookies: Vec<String> = response_headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.clone())
            .collect();
        store_response_cookies(context, &url, &set_cookies).await;

        if REDIRECT_STATUS.contains(&status) && redirects_left >= 0 {
            if redirects_left == 0 {
                return Err(Error::navigation("Max redirect count exceeded").into());
            }
            let location = find_header(&response_headers, "location").map(str::to_string);
            if let Some(location) = location.filter(|l| !l.is_empty()) {
                let next = url.join(&location).map_err(|_| {
                    Error::navigation(format!(
                        "uri requested responds with an invalid redirect URL: {location}"
                    ))
                })?;
                remove_header(&mut headers, "cookie");
                if ((status == 301 || status == 302) && method == "POST")
                    || (status == 303 && method != "GET" && method != "HEAD")
                {
                    method = "GET".to_string();
                    body = None;
                    for name in [
                        "content-encoding",
                        "content-language",
                        "content-length",
                        "content-location",
                        "content-type",
                    ] {
                        remove_header(&mut headers, name);
                    }
                }
                if next.origin() != url.origin() {
                    remove_header(&mut headers, "authorization");
                }
                redirects_left -= 1;
                url = next;
                continue;
            }
        }
        if status == 401 && find_header(&headers, "authorization").is_none() {
            let challenge = find_header(&response_headers, "www-authenticate").unwrap_or_default();
            if let Some(credentials) = matching_credentials(&defaults.http_credentials, &url) {
                if challenge.trim().starts_with("Basic") {
                    set_header(
                        &mut headers,
                        "authorization",
                        &basic_auth(credentials),
                        false,
                    );
                    continue;
                }
            }
        }
        let encoding = find_header(&response_headers, "content-encoding").map(str::to_lowercase);
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes.to_vec(),
            Err(err) => {
                let (message, reset) = describe_error(&err, &url);
                let message = match encoding {
                    Some(encoding) if !reset => {
                        format!("failed to decompress '{encoding}' encoding: {message}")
                    }
                    _ => message,
                };
                return Err(SendError {
                    error: Error::navigation(message),
                    reset,
                });
            }
        };
        // reqwest does not report the DNS/connect/TLS phases: they stay -1.
        let timing = crate::network::ResourceTiming {
            start_time,
            domain_lookup_start: -1.0,
            domain_lookup_end: -1.0,
            connect_start: -1.0,
            secure_connection_start: -1.0,
            connect_end: -1.0,
            request_start: 0.0,
            response_start,
            response_end: started.elapsed().as_secs_f64() * 1000.0,
        };
        return Ok(RawResponse {
            url: url.to_string(),
            status,
            status_text,
            headers: response_headers,
            body: bytes,
            log,
            server_addr,
            security_details,
            timing,
        });
    }
}

// ----------------------------------------------------------------------
// Responses
// ----------------------------------------------------------------------

/// Where the body of an [`APIResponse`] lives.
#[derive(Clone)]
enum BodySource {
    /// Kept by the request context until `dispose` (requests).
    Stored {
        context: APIRequestContext,
        uid: u64,
    },
    /// Owned by the response (`route.fetch`, [`APIResponse::new`]).
    Owned(Arc<Vec<u8>>),
}

/// The response of an [`APIRequestContext`] request or of `route.fetch()`.
/// Cheap to clone.
#[derive(Clone)]
pub struct APIResponse {
    body: BodySource,
    url: String,
    status: u16,
    status_text: String,
    headers: Vec<(String, String)>,
    log: Arc<Vec<String>>,
    server_addr: Option<crate::network::RemoteAddr>,
    security_details: Option<crate::network::SecurityDetails>,
    timing: crate::network::ResourceTiming,
}

impl std::fmt::Debug for APIResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("APIResponse")
            .field("url", &self.url)
            .field("status", &self.status)
            .finish()
    }
}

impl std::fmt::Display for APIResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "APIResponse: {} {}", self.status, self.status_text)?;
        for (name, value) in &self.headers {
            writeln!(f, "  {name}: {value}")?;
        }
        Ok(())
    }
}

impl APIResponse {
    /// Create a response (e.g. to fulfill a route with).
    pub fn new(status: u16, headers: Vec<Header>, body: impl Into<Vec<u8>>) -> Self {
        Self::owned(
            String::new(),
            status,
            crate::network::status_text(status).to_string(),
            headers,
            body.into(),
        )
    }

    /// A response holding its own body.
    pub(crate) fn owned(
        url: String,
        status: u16,
        status_text: String,
        headers: Vec<Header>,
        body: Vec<u8>,
    ) -> Self {
        APIResponse {
            body: BodySource::Owned(Arc::new(body)),
            url,
            status,
            status_text,
            headers: headers.into_iter().map(|h| (h.name, h.value)).collect(),
            log: Arc::default(),
            server_addr: None,
            security_details: None,
            timing: Default::default(),
        }
    }

    /// The request log (`apiResponse._fetchLog()`), used by `to_be_ok`.
    pub(crate) fn fetch_log(&self) -> Vec<String> {
        self.log.as_ref().clone()
    }

    /// IP address and port of the server that answered.
    pub async fn server_addr(&self) -> Option<crate::network::RemoteAddr> {
        self.server_addr.clone()
    }

    /// TLS details of an HTTPS response (subject, issuer and validity of
    /// the server certificate; `protocol` is not reported by the HTTP
    /// client and stays `None`). `None` for plain HTTP.
    pub async fn security_details(&self) -> Option<crate::network::SecurityDetails> {
        self.security_details.clone()
    }

    /// Timing of the request (milliseconds relative to `start_time`). The
    /// HTTP client does not report DNS, connect and TLS phases: those are
    /// -1, like unknown values in Playwright.
    pub fn timing(&self) -> crate::network::ResourceTiming {
        self.timing
    }

    /// Final URL (after redirects).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// HTTP status.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// HTTP status text.
    pub fn status_text(&self) -> &str {
        &self.status_text
    }

    /// Whether the status is in `200..=299`.
    pub fn ok(&self) -> bool {
        (200..=299).contains(&self.status)
    }

    /// Headers with lowercase names; repeated headers are joined with
    /// `", "` (`set-cookie` with `"\n"`).
    pub fn headers(&self) -> HashMap<String, String> {
        let mut out: HashMap<String, String> = HashMap::new();
        for (name, value) in &self.headers {
            let name = name.to_lowercase();
            let separator = if name == "set-cookie" { "\n" } else { ", " };
            out.entry(name)
                .and_modify(|v| {
                    v.push_str(separator);
                    v.push_str(value);
                })
                .or_insert_with(|| value.clone());
        }
        out
    }

    /// All headers, in order, with repeated headers kept apart.
    pub fn headers_array(&self) -> Vec<Header> {
        self.headers
            .iter()
            .map(|(name, value)| Header::new(name.clone(), value.clone()))
            .collect()
    }

    /// Value of a header (case-insensitive; repeated values joined).
    pub fn header_value(&self, name: &str) -> Option<String> {
        self.headers().get(&name.to_lowercase()).cloned()
    }

    /// All values of a header.
    pub fn header_values(&self, name: &str) -> Vec<String> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn body_bytes(&self) -> Result<Arc<Vec<u8>>> {
        let (context, uid) = match &self.body {
            BodySource::Owned(body) => return Ok(body.clone()),
            BodySource::Stored { context, uid } => (context, uid),
        };
        if context.responses_disposed() {
            return Err(Error::target_closed_with("Response has been disposed"));
        }
        context
            .inner
            .responses
            .lock()
            .unwrap()
            .get(uid)
            .cloned()
            .ok_or_else(|| Error::target_closed_with("Response has been disposed"))
    }

    /// The response body.
    pub async fn body(&self) -> Result<Vec<u8>> {
        self.body_bytes()
            .map(|b| b.as_ref().clone())
            .api("apiResponse.body")
    }

    /// The response body as text (UTF-8, lossy).
    pub async fn text(&self) -> Result<String> {
        self.body_bytes()
            .map(|b| String::from_utf8_lossy(&b).to_string())
            .api("apiResponse.text")
    }

    /// The response body parsed as JSON.
    pub async fn json<T: DeserializeOwned>(&self) -> Result<T> {
        async {
            let body = self.body_bytes()?;
            serde_json::from_slice(&body).map_err(|e| Error::serialization(e.to_string()))
        }
        .await
        .api("apiResponse.json")
    }

    /// Release the body.
    pub async fn dispose(&self) -> Result<()> {
        if let BodySource::Stored { context, uid } = &self.body {
            context.inner.responses.lock().unwrap().remove(uid);
        }
        Ok(())
    }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Fetch>();
    is_send::<APIResponse>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_helpers() {
        let mut headers = vec![("Content-Type".to_string(), "text/plain".to_string())];
        set_header(&mut headers, "content-type", "application/json", true);
        assert_eq!(headers[0].1, "text/plain");
        set_header(&mut headers, "content-type", "application/json", false);
        assert_eq!(headers[0].1, "application/json");
        assert!(is_json_content_type(&headers));
        set_header(&mut headers, "x", "1", false);
        remove_header(&mut headers, "CONTENT-TYPE");
        assert_eq!(headers, vec![("x".to_string(), "1".to_string())]);
        assert_eq!(
            cookie_store::parse_cookie_name_value(" a = b "),
            ("a".into(), "b".into())
        );
    }

    #[test]
    fn credentials_match_origin() {
        let url = url::Url::parse("http://localhost:8080/x").unwrap();
        let c = HttpCredentials::new("u", "p").origin("http://LOCALHOST:8080");
        assert!(matching_credentials(&[c], &url).is_some());
        let c = HttpCredentials::new("u", "p").origin("http://localhost:8081");
        assert!(matching_credentials(&[c], &url).is_none());
        assert_eq!(
            basic_auth(&HttpCredentials::new("user", "pass")),
            "Basic dXNlcjpwYXNz"
        );
    }
}

// ----------------------------------------------------------------------
// Certificate details (minimal DER reader)
// ----------------------------------------------------------------------

/// One DER TLV: (tag, content, rest).
fn der_next(data: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *data.first()?;
    let first = *data.get(1)? as usize;
    let (len, header) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *data.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let content = data.get(header..header + len)?;
    Some((tag, content, &data[header + len..]))
}

/// The common name (or the last attribute) of an X.509 `Name`.
fn der_common_name(name: &[u8]) -> Option<String> {
    const CN: &[u8] = &[0x55, 0x04, 0x03];
    let mut rest = name;
    let mut fallback = None;
    while let Some((_, set, next)) = der_next(rest) {
        rest = next;
        let Some((_, attr, _)) = der_next(set) else {
            continue;
        };
        let Some((_, oid, value)) = der_next(attr) else {
            continue;
        };
        let Some((_, text, _)) = der_next(value) else {
            continue;
        };
        let text = String::from_utf8_lossy(text).into_owned();
        if oid == CN {
            return Some(text);
        }
        fallback = Some(text);
    }
    fallback
}

/// UTCTime / GeneralizedTime → Unix seconds.
fn der_time(tag: u8, content: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(content).ok()?;
    let s = s.strip_suffix('Z')?;
    let (year, rest) = if tag == 0x17 {
        let y: i64 = s.get(0..2)?.parse().ok()?;
        (if y >= 50 { 1900 + y } else { 2000 + y }, s.get(2..)?)
    } else {
        (s.get(0..4)?.parse().ok()?, s.get(4..)?)
    };
    let num = |r: std::ops::Range<usize>| -> Option<i64> { rest.get(r)?.parse().ok() };
    let (month, day, hour, minute) = (num(0..2)?, num(2..4)?, num(4..6)?, num(6..8)?);
    let second = num(8..10).unwrap_or(0);
    // Days from civil (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + hour * 3600 + minute * 60 + second) as f64)
}

fn certificate_details(der: &[u8]) -> crate::network::SecurityDetails {
    let mut details = crate::network::SecurityDetails::default();
    let mut parse = || -> Option<()> {
        let (_, certificate, _) = der_next(der)?;
        let (_, tbs, _) = der_next(certificate)?;
        let mut rest = tbs;
        let (tag, _, next) = der_next(rest)?;
        if tag == 0xa0 {
            rest = next; // version
        }
        let (_, _, next) = der_next(rest)?; // serial
        let (_, _, next) = der_next(next)?; // signature algorithm
        let (_, issuer, next) = der_next(next)?;
        let (_, validity, next) = der_next(next)?;
        let (_, subject, _) = der_next(next)?;
        details.issuer = der_common_name(issuer);
        details.subject_name = der_common_name(subject);
        let (t1, from, after) = der_next(validity)?;
        let (t2, to, _) = der_next(after)?;
        details.valid_from = der_time(t1, from);
        details.valid_to = der_time(t2, to);
        Some(())
    };
    let _ = parse();
    details
}

#[cfg(test)]
mod certificate_tests {
    #[test]
    fn parses_der_times() {
        assert_eq!(super::der_time(0x17, b"230811000000Z"), Some(1691712000.0));
        assert_eq!(
            super::der_time(0x18, b"20330807000000Z"),
            Some(2006985600.0)
        );
    }
}

/// Installs rustls' ring provider as the process default when none is set
/// yet (reqwest is built without a provider of its own). A provider the
/// application already installed is kept.
pub(crate) fn ensure_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}
