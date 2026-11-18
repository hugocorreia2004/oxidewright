//! # oxidewright
//!
//! A Rust port of the [Playwright](https://playwright.dev) API for Chromium,
//! built on top of [`chromiumoxide`], that leaves no protocol traces page
//! scripts can observe (no `Runtime.enable`, isolated worlds created on
//! demand). Class and method names follow Playwright's (in `snake_case`);
//! options are builder methods chained before `.await`.
//!
//! ## Quick start
//!
//! ```no_run
//! use oxidewright::{GetByRoleOptions, LoadState, chromium};
//!
//! #[tokio::main]
//! async fn main() -> oxidewright::Result<()> {
//!     // const browser = await chromium.launch();
//!     let browser = chromium().launch().headless(true).await?;
//!     // const context = await browser.newContext({ locale: 'en-US' });
//!     let context = browser.new_context().locale("en-US").await?;
//!     let page = context.new_page().await?;
//!
//!     // await page.goto(url, { waitUntil: 'domcontentloaded' });
//!     page.goto("https://example.com")
//!         .wait_until(LoadState::DomContentLoaded)
//!         .await?;
//!
//!     // await page.getByRole('link', { name: 'More information' }).click();
//!     page.get_by_role_with("link", GetByRoleOptions::new().name("More information"))
//!         .click()
//!         .await?;
//!
//!     // const title = await page.evaluate(() => document.title);
//!     let title: String = page.evaluate("() => document.title").await?;
//!     // await page.evaluate(([a, b]) => a + b, [1, 2]);
//!     let sum: i32 = page.evaluate("([a, b]) => a + b").arg((1, 2)).await?;
//!     println!("{title} {sum}");
//!
//!     // await page.screenshot({ path: 'page.png', fullPage: true });
//!     page.screenshot().path("page.png").full_page(true).await?;
//!     browser.close().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Where to start
//!
//! * [`chromium()`] → [`BrowserType::launch`] / [`BrowserType::connect_over_cdp`]
//!   → [`Browser`] → [`Browser::new_context`] → [`BrowserContext::new_page`]
//!   → [`Page`].
//! * Find elements with [`Page::locator`] and the `get_by_*` methods
//!   ([`Locator`]); actions auto-wait like Playwright's ([`actions`]).
//! * Network: [`Page::route`] ([`Route`]), [`Request`], [`Response`],
//!   [`Page::wait_for_response`], HAR ([`RecordHar`], [`Page::route_from_har`])
//!   and [`APIRequestContext`].
//! * Events: `page.on_*` closures, `wait_for_*` waiters and `expect_*`
//!   helpers ([`WaitForEvent`], [`EventKind`]).
//! * Errors: [`Error`], whose messages mirror Playwright's.
//!
//! The repository has runnable examples in `oxidewright/examples` and a
//! Playwright → oxidewright guide in `oxidewright/docs/from-playwright.md`.
//!
//! ## Conventions
//!
//! | Playwright | oxidewright |
//! |---|---|
//! | `page.waitForLoadState()` | `page.wait_for_load_state().await?` |
//! | `{ timeout: 5000, force: true }` | `.timeout(5_000).force(true)` before `.await` |
//! | `page.evaluate(fn, arg)` | `page.evaluate::<T>(js).arg(arg).await?` (serde) |
//! | `page.on('request', fn)` | `page.on_request(\|r\| ..)` |
//! | `Promise.all([page.waitForEvent('popup'), click])` | `page.expect_popup(click).await?` |
//! | `/text/i` | `oxidewright::regex("text", "i")` |
//! | `keyboard.type`, `mouse.move`, `route.continue` | `r#type`/`type_text`, `r#move`/`move_to`, `continue_` |
//!
//! ## Stealth
//!
//! The `stealth` cargo feature (on by default) builds chromiumoxide without
//! `Runtime.enable`, `Log.enable` and `Performance.enable`. oxidewright never
//! needs them. Features that are observable by pages are opt-in:
//! console/pageerror events (`.console_events(true)` on the context), JS/CSS
//! coverage ([`coverage`]) and emulation overrides ([`emulation`]), which are
//! only sent when the corresponding option is set.
//!
//! ## Design
//!
//! * Execution contexts are created on demand with `Page.createIsolatedWorld`
//!   and the main world is reached through its `globalThis` object, so
//!   `Runtime.enable` is never needed (see [`execution_context`]).
//! * Values cross the boundary with Playwright's own serializer
//!   (`UtilityScript`), so `undefined`, `NaN`, `-0`, dates, regular
//!   expressions, `BigInt`s and handles round-trip like in Playwright
//!   (see [`JsValue`]).
//! * Frame lifecycle (`commit`, `domcontentloaded`, `load`, `networkidle`) is
//!   tracked like Playwright's `server/frames.ts`.
//! * The JavaScript bundles (`InjectedScript`, `UtilityScript`, ...) come
//!   from Patchright's build of Playwright, see [`js::sources`].

#![warn(missing_docs)]
// CDP parameter structs are built with `Default` and then filled in.
#![allow(clippy::field_reassign_with_default)]

pub mod actions;
mod aria_snapshot;
mod binding;
mod browser;
mod browser_context;
mod browser_type;
mod cdp_raw;
mod cdp_session;
mod chromium_tracing;
mod clock;
mod console;
mod context_ext;
mod context_options;
mod cookies;
pub mod coverage;
mod credentials;
mod dialog;
mod download;
mod drop_action;
mod element_actions;
pub mod emulation;
mod error;
mod events;
pub mod execution_context;
pub mod expect;
mod ext;
pub mod fetch;
mod file_chooser;
mod frame;
mod frame_selectors;
mod frame_wait;
mod har;
pub mod human;
pub mod input;
pub mod js;
mod js_handle;
mod locator;
mod locator_generators;
mod locator_utils;
mod network;
mod network_manager;
mod page;
mod page_ext;
mod page_extras;
mod page_state;
mod page_tools;
mod pdf;
mod progress;
mod proxy;
mod route;
mod screencast;
mod screencast_api;
mod screenshot;
mod selector_parser;
mod selectors;
mod service_worker;
mod storage_state;
pub mod trace;
mod url_match;
pub mod video;
mod web_socket_route;
mod websocket;
mod worker;

pub use actions::{
    Action, BoundingBox, Click, Dblclick, ElementState, Fill, Hover, InputFiles, Position, Press,
    QuerySelector, SelectOption, SelectOptionAction, SelectValue, SelectValues, SetChecked,
    SetInputFiles, Tap, Type, WaitForSelector, kinds,
};
pub use aria_snapshot::PageAriaSnapshot;
pub use binding::{BindingError, BindingSource, ExposedBinding};
pub use browser::{Browser, CloseBrowser};
pub use browser_context::{BrowserContext, NewContext, NewContextOptions, NewPage};
pub use browser_type::{
    BrowserType, ConnectOverCdp, IgnoreDefaultArgs, Launch, LaunchOptions, LaunchPersistentContext,
    chromium,
};
pub use cdp_session::{CDPSession, CdpSend, CdpSessionTarget};
pub use chromium_tracing::StartTracing;
/// An event of a CDP session, see [`CDPSession::all_events`].
pub use chromiumoxide::listeners::AnyEvent as CdpEvent;
pub use clock::{Clock, ClockInstall, ClockTicks, ClockTime};
pub use console::{ConsoleMessage, ConsoleMessageLocation, WebError};
pub use context_options::{Device, devices};
pub use cookies::{ClearCookies, Cookie, Cookies, SameSite, SetCookie};
pub use coverage::{
    CoverageRange, CssCoverageEntry, FunctionCoverage, JsCoverageEntry, StartCssCoverage,
    StartJsCoverage, TextRange,
};
pub use credentials::{CreateCredential, Credentials, GetCredentials, VirtualCredential};
pub use dialog::Dialog;
pub use download::Download;
pub use drop_action::DropPayload;
pub use element_actions::WaitForSelectorState;
pub use emulation::{
    ColorScheme, Contrast, EmulateMedia, ForcedColors, Geolocation, GrantPermissions, Media,
    ReducedMotion, ServiceWorkerPolicy,
};
pub use error::{CONTEXT_DESTROYED_MESSAGE, Error, Result, TARGET_CLOSED_MESSAGE};
pub use events::{Event, EventKind, ListenerId, WaitForEvent};
pub use execution_context::{ExecutionContext, World, set_default_isolated_context};
pub use fetch::{
    APIRequest, APIRequestContext, APIResponse, ClientCertificate, FilePayload,
    HttpCredentialsSend, MultipartValue, ProxySettings, RequestData, request,
};
pub use file_chooser::{FileChooser, SetFiles};
pub use frame::{
    Frame, FrameEvaluate, FrameEvaluateHandle, Goto, LoadState, SetContent, WaitForLoadState,
    WaitForNavigation, WaitForUrl,
};
pub use frame_wait::{Polling, WaitForFunction};
pub use har::{HarContentPolicy, HarMode, HarNotFound, RecordHar, RouteFromHar, StartHar};
pub use input::{
    Keyboard, KeyboardModifier, KeyboardPress, KeyboardType, Mouse, MouseButton, MouseButtonAction,
    MouseClick, MouseMove, Touchscreen,
};
pub use js::JsValue;
pub use js_handle::{ElementHandle, EvaluateHandleOn, EvaluateOn, JSHandle};
pub use locator::{
    FrameLocator, GetByOptions, GetByRoleOptions, Highlight, Locator, LocatorOptions,
    SelectorOrLocator,
};
pub use locator_utils::{JsRegex, TextMatch, regex};
pub use network::{
    Header, RemoteAddr, Request, RequestSizes, ResourceTiming, Response, SecurityDetails,
};
pub use network_manager::HttpCredentials;
pub use page::{AddInitScript, GoHistory, InitScript, Page, Reload, ViewportSize};
pub use page_extras::{AddTag, CloseContext, ClosePage};
pub use page_state::{AddLocatorHandler, MessagesFilter};
pub use page_tools::{Content, WebStorage};
pub use pdf::{Pdf, PdfLength, PdfMargin};
pub use progress::DEFAULT_TIMEOUT;
pub use route::{
    Abort, Continue, Fallback, Fetch, Fulfill, Route, RouteBuilder, RouteHandlerFn, RouteId,
    Unroute, UnrouteAll, UnrouteBehavior,
};
pub use screencast_api::{Screencast, ScreencastFrame, StartScreencast};
pub use screenshot::{
    Animations, Caret, ElementScreenshot, Mask, PageScreenshot, Rect, ScreenshotScale,
    ScreenshotType,
};
pub use selectors::{RegisterSelector, Selectors, selectors};
pub use service_worker::WaitForServiceWorker;
pub use storage_state::{
    NameValue, OriginState, StorageState, StorageStateBuilder, StorageStateSource,
};
pub use trace::{Tracing, TracingStart, TracingStop};
pub use url_match::{UrlMatch, glob_to_regex_pattern};
pub use video::Video;
pub use web_socket_route::{WebSocketRoute, WebSocketRouteServer};
pub use websocket::{WebSocket, WebSocketEvent, WebSocketPayload};
pub use worker::{Worker, WorkerEvaluate, WorkerEvaluateHandle};

/// Re-export of the underlying CDP library.
pub use chromiumoxide;
