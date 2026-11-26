# Coming from Playwright (JS/TS)

oxidewright keeps Playwright's classes, method names, option names, defaults and error
messages. This guide covers the conventions that change in Rust, then lists every method of
every class next to its Rust form.

## Conventions

### Names

Methods are `snake_case`: `page.waitForLoadState()` is `page.wait_for_load_state()`.
Acronyms are lowercased as words (`setExtraHTTPHeaders` → `set_extra_http_headers`,
`newCDPSession` → `new_cdp_session`). Names that are Rust keywords have a raw form and an
alias:

| Playwright | Rust |
|---|---|
| `type` | `r#type` or `type_text` (`Keyboard`, `ElementHandle`, `Page`, `Frame`); `Locator::r#type` (prefer `press_sequentially`) |
| `mouse.move` | `mouse.r#move` or `mouse.move_to` |
| `route.continue` | `route.continue_` |
| `locator.and` / `locator.or` | `locator.and_` / `locator.or_` |
| `dialog.type()`, `message.type()` | `dialog.r#type()`, `message.r#type()` |

### Options are builder methods

An options object becomes builder methods chained before `.await`. The builders implement
`IntoFuture`, so nothing runs until they are awaited:

```js
await page.locator('#a').click({ button: 'right', timeout: 5000, force: true });
await page.goto(url, { waitUntil: 'domcontentloaded' });
const browser = await chromium.launch({ headless: false, args: ['--lang=en-US'] });
```

```rust,ignore
page.locator("#a").click().button(MouseButton::Right).timeout(5_000).force(true).await?;
page.goto(url).wait_until(LoadState::DomContentLoaded).await?;
let browser = chromium().launch().headless(false).args(["--lang=en-US"]).await?;
```

A builder only has the options its action accepts (`click` has `button`, `fill` does not).
String unions become enums (`'domcontentloaded'` → `LoadState::DomContentLoaded`, `'right'`
→ `MouseButton::Right`). Timeouts are `u64` milliseconds. Optional overloads become `_with`
variants taking an options value: `page.getByRole('button', { name: 'OK' })` is
`page.get_by_role_with("button", GetByRoleOptions::new().name("OK"))`, and
`page.locator('li', { hasText: 'x' })` is
`page.locator_with("li", LocatorOptions::new().has_text("x"))`.

### Errors

Every fallible call returns `oxidewright::Result<T>`; the error is the `oxidewright::Error`
enum. Its `Display` is Playwright's message, including the call log:

```text
locator.click: Timeout 30000ms exceeded.
Call log:
  - waiting for locator('#missing')
```

```rust,ignore
match page.locator("#missing").click().timeout(1_000).await {
    Err(e) if e.is_timeout() => println!("timed out: {e}"),   // errors.TimeoutError
    Err(e) if e.is_target_closed() => println!("page closed"),
    Err(e) => return Err(e),
    Ok(()) => {}
}
```

Variants: `Timeout`, `TargetClosed`, `Navigation`, `Evaluation`, `FrameDetached`,
`StrictModeViolation`, `InvalidArgument`, `Serialization`, `Launch`, `Protocol`. The enum is
`#[non_exhaustive]`.

### Evaluate

The JavaScript is a string. As in Playwright, if it evaluates to a function, the function is
called with the argument. Arguments are any `serde::Serialize` value (tuples become arrays,
structs and maps become objects, and `JSHandle`/`ElementHandle` values become handles).
Results deserialize into any `serde::de::DeserializeOwned` type:

```js
const sum = await page.evaluate(([a, b]) => a + b, [1, 2]);
const href = await page.locator('a').evaluate(a => a.href);
const handle = await page.evaluateHandle(() => document.body);
```

```rust,ignore
let sum: i32 = page.evaluate("([a, b]) => a + b").arg((1, 2)).await?;
let href: String = page.locator("a").evaluate("a => a.href").await?;
let handle = page.evaluate_handle("() => document.body").await?;
let tag: String = page.evaluate("b => b.tagName").arg(&handle).await?;
```

Values use Playwright's own serializer, so `undefined`, `NaN`, `-0`, `Infinity`, `Date`,
`RegExp`, `URL`, `BigInt`, `Map`/`Set`, errors and typed arrays survive the trip. Deserialize
into `oxidewright::JsValue` to keep those distinctions; deserialize into
`serde_json::Value` to get plain JSON. `page.evaluate` runs in the page's main world;
`.isolated_context(true)` runs it in oxidewright's isolated world instead (page scripts cannot
see its globals).

### Events

Handlers are closures. Each `on` call returns a `ListenerId` to remove it later; there are
typed variants for every event:

```js
page.on('request', request => console.log(request.url()));
page.once('dialog', dialog => dialog.accept());
```

```rust,ignore
let id = page.on_request(|request| println!("{}", request.url()));
page.remove_listener(id);
page.once(EventKind::Dialog, |event| {
    if let Event::Dialog(dialog) = event {
        tokio::spawn(async move { dialog.accept(None).await });
    }
});
```

Handlers are synchronous and run in order on a task of the page; spawn a task for async work.
A dialog with no listener is dismissed, like in Playwright.

### Waiting for events

`wait_for_*` methods subscribe when they are *called* and resolve when awaited, so create the
waiter before triggering the event. `expect_*` helpers run an action and wait at the same
time:

```js
const responsePromise = page.waitForResponse('**/api/items');
await page.getByText('Load more').click();
const response = await responsePromise;

const [popup] = await Promise.all([page.waitForEvent('popup'), page.click('a')]);
```

```rust,ignore
let response = page.wait_for_response("**/api/items");
page.get_by_text("Load more").click().await?;
let response = response.await?;

let popup = page.expect_popup(page.click("a")).await?;
let response = page
    .wait_for_response("**/api/*")
    .predicate(|r| r.status() == 200)
    .timeout(10_000)
    .await?;
```

### URL and text matching

URL arguments (`route`, `wait_for_url`, `wait_for_request`, ...) take a glob string
(`"**/api/*"`), a `regex::Regex` or `UrlMatch::predicate(|url| ..)`. Text arguments
(`get_by_text`, `has_text`, ...) take a string or a JavaScript regular expression built with
`oxidewright::regex("hello", "i")` (that is `/hello/i`); a `regex::Regex` is also accepted.

### Handles and cloning

`Browser`, `BrowserContext`, `Page`, `Frame`, `Locator`, `JSHandle` and `ElementHandle` are
cheap `Clone` values: clone them into closures and tasks instead of sharing references.
Locators are lazy, strict and re-resolved on every action, as in Playwright.

### Routes

```js
await page.route('**/api/**', async route => {
  const response = await route.fetch();
  await route.fulfill({ response, json: { patched: true } });
});
```

```rust,ignore
page.route("**/api/**", |route: Route| async move {
    let response = route.fetch().await?;
    route.fulfill().response(&response).json(&json!({ "patched": true })).await
})
.await?;
```

The handler returns `oxidewright::Result<()>`. Routes added later run first; `fallback()`
passes the request to the previous route.

## Class by class

"missing" means there is no equivalent yet. Rust calls show the arguments by name; options
are omitted unless they illustrate the builder.

### Browser

| Playwright | oxidewright | Notes |
|---|---|---|
| `browser.removeAllListeners()` | `browser.remove_all_listeners(None)` | or `Some(EventKind::Context)` |
| `browser.on()` | `browser.on(EventKind::Disconnected, \|event\| ..)` | typed: `on_context`, `on_disconnected` |
| `browser.once()` | `browser.once(EventKind::Context, \|event\| ..)` |  |
| `browser.removeListener()` | `browser.remove_listener(id)` |  |
| `browser.bind()` | missing | not implemented yet |
| `browser.browserType()` | `browser.browser_type()` |  |
| `browser.close()` | `browser.close().reason("done").await` | every context closes too (`close` events, `close_reason`) |
| `browser.contexts()` | `browser.contexts()` |  |
| `browser.isConnected()` | `browser.is_connected()` |  |
| `browser.newBrowserCDPSession()` | `browser.new_browser_cdp_session().await` |  |
| `browser.newContext()` | `browser.new_context().locale("en-US").await` | every context option is a builder method |
| `browser.newPage()` | `browser.new_page().locale("de-DE").await` | every context option; the page owns its context (closing the page closes it, `context.new_page()` fails) |
| `browser.startTracing(page, { path, screenshots, categories })` | `browser.start_tracing().page(&page).path(p).screenshots(true).categories([..])` | Chromium performance trace (DevTools Performance panel) |
| `browser.stopTracing()` | `browser.stop_tracing()` | returns the trace JSON bytes |
| `browser.unbind()` | missing | not implemented yet |
| `browser.version()` | `browser.version()` |  |

Events: `context` (`Event::Context`) and `disconnected` (`Event::Disconnected`, also after
`close()`); `browser.wait_for_event(kind)`, `wait_for_context()`.

Also: `with_cdp`.

### BrowserType

| Playwright | oxidewright | Notes |
|---|---|---|
| `chromium.connectOverCDP()` | `chromium().connect_over_cdp(endpoint).headers(h).slow_mo(100).await` | `timeout`, `headers`, `slow_mo`, `no_defaults` (keeps the default context's download behavior), `is_local` (accepted, no effect); `artifactsDir` is not supported |
| `chromium.connect()` | missing | Playwright server protocol; use `connect_over_cdp` |
| `chromium.executablePath()` | `chromium().executable_path()?` | the detected Chrome/Chromium/Edge (no bundled browser) |
| `chromium.launch()` | `chromium().launch().headless(true).await` | see the options below |
| `chromium.launchPersistentContext()` | `chromium().launch_persistent_context(dir).locale("de-DE").await` | every launch and context option; returns the default context; closing it closes the browser |
| `chromium.launchServer()` | missing | not implemented yet |
| `chromium.name()` | `chromium().name()` |  |

Launch options: `headless`, `executable_path`, `channel` (`chrome`, `chrome-beta`, `chrome-dev`,
`chrome-canary`, `msedge`, `msedge-beta`, `msedge-dev`, `msedge-canary`, found in their standard
install locations like Playwright's registry), `args`, `ignore_default_args(true)` or
`ignore_default_args(["--mute-audio"])`, `chromium_sandbox`, `timeout`, `env`, `proxy`, `slow_mo`
(input actions, navigations and history calls, like Playwright), `downloads_path`; `traces_dir` and
`handle_sigint`/`handle_sigterm`/`handle_sighup` are accepted for compatibility but have no effect
(traces are kept in memory; no signal handler is installed, the browser process is killed when the
`Browser` is closed or dropped). `artifactsDir` and `logger` are not supported. The extra
`user_data_dir` option of `launch()` still exists (Playwright rejects it there).

### BrowserContext

| Playwright | oxidewright | Notes |
|---|---|---|
| `context.exposeBinding()` | `context.expose_binding(name, \|source, args\| async move { .. }).await` | Patchright's way: only `Runtime.addBinding(name)`, no `Runtime.enable`; returns `ExposedBinding` (`dispose()`) |
| `context.addInitScript()` | `context.add_init_script(script).await` |  |
| `context.removeAllListeners()` | `context.remove_all_listeners(None)` | or `Some(EventKind::Page)` |
| `context.on()` | `context.on(EventKind::Page, \|event\| ..)` | or typed: `context.on_page(\|p\| ..)` |
| `context.once()` | `context.once(EventKind::Page, \|event\| ..)` |  |
| `context.removeListener()` | `context.remove_listener(id)` |  |
| `context.addCookies()` | `context.add_cookies(cookies).await` |  |
| `context.backgroundPages()` | missing | extensions are not supported |
| `context.browser()` | `context.browser()` |  |
| `context.clearCookies()` | `context.clear_cookies().name("a").await` | filters: `name`, `domain`, `path` (string or regex) |
| `context.clearPermissions()` | `context.clear_permissions().await` |  |
| `context.close()` | `context.close().await` |  |
| `context.cookies()` | `context.cookies().urls(urls).await` |  |
| `context.exposeFunction()` | `context.expose_function(name, \|args\| async move { Ok::<_, BindingError>(..) }).await` | args are `Vec<JsValue>`; any `Serialize` result (handles included) |
| `context.grantPermissions()` | `context.grant_permissions(permissions).await` |  |
| `context.isClosed()` | `context.is_closed()` |  |
| `context.newCDPSession()` | `context.new_cdp_session(&page).await` | or `&frame` |
| `context.newPage()` | `context.new_page().await` | a tab of the context's window, as in Playwright |
| — | `context.new_window_page().await` | not in Playwright: a page of the context in a window of its own, which keeps full rate while another page is in front (some Chromium builds with a visible window draw a background tab at about one frame a second, and every mouse move waits for one) |
| `context.pages()` | `context.pages()` |  |
| `context.route()` | `context.route(url, \|route\| async move { .. }).await` |  |
| `context.routeFromHAR()` | `context.route_from_har(path).await` |  |
| `context.routeWebSocket()` | `context.route_web_socket(url, \|ws: WebSocketRoute\| async move { .. }).await` | same main-world mock and globals as Patchright, only while routing |
| `context.serviceWorkers()` | `context.service_workers()` | `Worker`s; evaluated with `Runtime.evaluate` only |
| `context.setDefaultNavigationTimeout()` | `context.set_default_navigation_timeout(timeout_ms)` |  |
| `context.setDefaultTimeout()` | `context.set_default_timeout(timeout_ms)` |  |
| `context.setExtraHTTPHeaders()` | `context.set_extra_http_headers(headers).await` |  |
| `context.setGeolocation()` | `context.set_geolocation(geolocation).await` |  |
| `context.setHTTPCredentials()` | `context.set_http_credentials(credentials).await` |  |
| `context.setOffline()` | `context.set_offline(offline).await` |  |
| `context.setStorageState()` | `context.set_storage_state(state).await` |  |
| `context.storageState()` | `context.storage_state().path(path).await` |  |
| `context.unroute()` | `context.unroute(url).await` |  |
| `context.unrouteAll()` | `context.unroute_all().await` |  |
| `context.waitForEvent()` | `context.wait_for_event(EventKind::Page).await` | typed: `wait_for_page`, `wait_for_request`, ...; `expect_page(action)` |
| `context.tracing` | `context.tracing()` |  |
| `context.clock` | `context.clock()` |  |
| `context.request` | `context.request()` |  |
| `context.credentials` | `context.credentials()` | Chromium's virtual authenticator (CDP `WebAuthn`), nothing injected in the page |

Typed event handlers: `on_page`, `on_console`, `on_dialog`, `on_dialog_closed`, `on_web_error`, `on_request`, `on_request_failed`, `on_request_finished`, `on_response`, `on_download`, `on_frame_attached`, `on_frame_detached`, `on_frame_navigated`, `on_page_close`, `on_page_load`, `on_close`, `on_service_worker` (the `serviceworker` event; `wait_for_service_worker()` waits for one).

Also: `wait_for_page`, `wait_for_request`, `wait_for_response`, `wait_for_console_message`, `wait_for_dialog`, `wait_for_dialog_closed`, `wait_for_close`, `expect_event`, `expect_page`, `expect_console_message`, `close_reason`.

### Page

| Playwright | oxidewright | Notes |
|---|---|---|
| `page.$()` | `page.query_selector(selector).strict(b).await` |  |
| `page.$$()` | `page.query_selector_all(selector).await` |  |
| `page.$eval()` | `page.eval_on_selector(selector, expression).await` |  |
| `page.$$eval()` | `page.eval_on_selector_all(selector, expression).await` |  |
| `page.evaluate()` | `page.evaluate::<T>(expression).arg(arg).await` | main world; `.isolated_context(true)` for the utility world |
| `page.evaluateHandle()` | `page.evaluate_handle(expression).await` |  |
| `page.addInitScript()` | `page.add_init_script(script).await` | `add_init_script_path(path)` for `{ path }`; returns an `InitScript` with `dispose()` |
| `page.waitForFunction()` | `page.wait_for_function(expression).arg(a).polling(100).await` | `.polling("raf")` (default) or ms; isolated world by default with `stealth`, `.isolated_context(false)` for page globals |
| `page.waitForSelector()` | `page.wait_for_selector(selector).await` |  |
| `page.exposeBinding()` | `page.expose_binding(name, \|source, args\| async move { .. }).await` | `source.page`, `source.frame`, `source.context` |
| `page.removeAllListeners()` | `page.remove_all_listeners(Some(EventKind::Request))` | `None` removes all |
| `page.on()` | `page.on(EventKind::Request, \|event\| ..)` | or typed: `page.on_request(\|r\| ..)`; returns a `ListenerId` |
| `page.once()` | `page.once(EventKind::Popup, \|event\| ..)` |  |
| `page.removeListener()` | `page.remove_listener(id)` | takes the `ListenerId` returned by `on`/`on_*` |
| `page.addLocatorHandler()` | `page.add_locator_handler(&locator, \|l\| async move { .. }).no_wait_after(b).times(n).await` | runs before actions and auto-waiting assertions |
| `page.addScriptTag()` | `page.add_script_tag().await` |  |
| `page.addStyleTag()` | `page.add_style_tag().await` |  |
| `page.ariaSnapshot()` | `page.aria_snapshot().mode("ai").depth(n).boxes(true).await` |  |
| `page.ariaSnapshotJSON()` | `page.aria_snapshot_json().await` | a `serde_json::Value` |
| `page.bringToFront()` | `page.bring_to_front().await` |  |
| `page.cancelPickLocator()` | missing | no inspector |
| `page.check()` | `page.check(selector).await` |  |
| `page.clearConsoleMessages()` | `page.clear_console_messages().await` |  |
| `page.clearPageErrors()` | `page.clear_page_errors().await` |  |
| `page.click()` | `page.click(selector).await` |  |
| `page.close()` | `page.close().await` |  |
| `page.consoleMessages()` | `page.console_messages().await` / `console_messages_with(MessagesFilter::All)` | needs `console_events(true)` on the context |
| `page.content()` | `page.content().include_shadow(b).await` |  |
| `page.context()` | `page.context()` |  |
| `page.dblclick()` | `page.dblclick(selector).await` |  |
| `page.dispatchEvent()` | `page.dispatch_event(selector, type_).await` |  |
| `page.dragAndDrop()` | `page.drag_and_drop(source, target).scroll(b).await` | `scroll: "none"` is `.scroll(false)` |
| `page.emulateMedia()` | `page.emulate_media().await` |  |
| `page.exposeFunction()` | `page.expose_function(name, \|args\| async move { .. }).await` | throw in the page with `Err(BindingError::new(msg))` or `BindingError::value(v)` |
| `page.fill()` | `page.fill(selector, value).await` |  |
| `page.focus()` | `page.focus(selector).await` |  |
| `page.frame()` | `page.frame(name)` |  |
| `page.frameLocator()` | `page.frame_locator(selector)` |  |
| `page.frames()` | `page.frames()` |  |
| `page.getAttribute()` | `page.get_attribute(selector, name).await` |  |
| `page.getByAltText()` | `page.get_by_alt_text(text)` |  |
| `page.getByLabel()` | `page.get_by_label(text)` |  |
| `page.getByPlaceholder()` | `page.get_by_placeholder(text)` |  |
| `page.getByRef(ref)` | `page.get_by_ref("e3")` | refs of the last `aria_snapshot().mode("ai")` |
| `page.getByRole()` | `page.get_by_role(role)` | `get_by_role_with(role, GetByRoleOptions::new().name(..))` for options |
| `page.getByTestId()` | `page.get_by_test_id(test_id)` |  |
| `page.getByText()` | `page.get_by_text(text)` | `get_by_text_with(text, GetByOptions::exact())`; same `_with` pattern for alt text, label, placeholder, title |
| `page.getByTitle()` | `page.get_by_title(text)` |  |
| `page.goBack()` | `page.go_back().await` |  |
| `page.goForward()` | `page.go_forward().await` |  |
| `page.goto()` | `page.goto(url).wait_until(LoadState::Load).await` | returns `Option<Response>` |
| `page.hideHighlight()` | `page.hide_highlight().await` |  |
| `page.hover()` | `page.hover(selector).await` |  |
| `page.innerHTML()` | `page.inner_html(selector).await` |  |
| `page.innerText()` | `page.inner_text(selector).await` |  |
| `page.inputValue()` | `page.input_value(selector).await` |  |
| `page.isChecked()` | `page.is_checked(selector).await` |  |
| `page.isClosed()` | `page.is_closed()` |  |
| `page.isDisabled()` | `page.is_disabled(selector).await` |  |
| `page.isEditable()` | `page.is_editable(selector).await` |  |
| `page.isEnabled()` | `page.is_enabled(selector).await` |  |
| `page.isHidden()` | `page.is_hidden(selector).await` |  |
| `page.isVisible()` | `page.is_visible(selector).await` |  |
| `page.locator()` | `page.locator(selector)` | `locator_with(selector, LocatorOptions::new().has_text(..))` for options |
| `page.mainFrame()` | `page.main_frame()` |  |
| `page.opener()` | `page.opener()` |  |
| `page.pageErrors()` | `page.page_errors().await` / `page_errors_with(filter)` | needs `console_events(true)` on the context |
| `page.pause()` | missing | no inspector |
| `page.pdf()` | `page.pdf().await` |  |
| `page.pickLocator()` | missing | no inspector |
| `page.press()` | `page.press(selector, key).await` |  |
| `page.reload()` | `page.reload().await` |  |
| `page.removeLocatorHandler()` | `page.remove_locator_handler(&locator).await` |  |
| `page.requestGC()` | `page.request_gc().await` |  |
| `page.requests()` | `page.requests().await` | last 100 requests |
| `page.route()` | `page.route(url, \|route\| async move { .. }).times(n).await` | returns a `RouteId` for `unroute(url).handler(id)` |
| `page.routeFromHAR()` | `page.route_from_har(path).await` |  |
| `page.routeWebSocket()` | `page.route_web_socket(url, \|ws: WebSocketRoute\| async move { .. }).await` | page handlers win over context ones |
| `page.screenshot()` | `page.screenshot().path(path).full_page(true).await` | returns the bytes |
| `page.selectOption()` | `page.select_option(selector, values).await` |  |
| `page.setChecked()` | `page.set_checked(selector, checked).await` |  |
| `page.setContent()` | `page.set_content(html).await` |  |
| `page.setDefaultNavigationTimeout()` | `page.set_default_navigation_timeout(timeout_ms)` |  |
| `page.setDefaultTimeout()` | `page.set_default_timeout(timeout_ms)` |  |
| `page.setExtraHTTPHeaders()` | `page.set_extra_http_headers(headers).await` |  |
| `page.setInputFiles()` | `page.set_input_files(selector, files).await` |  |
| `page.setViewportSize()` | `page.set_viewport_size(viewport).await` |  |
| `page.tap()` | `page.tap(selector).await` |  |
| `page.textContent()` | `page.text_content(selector).await` |  |
| `page.title()` | `page.title().await` |  |
| `page.type()` | `page.r#type(selector, text).await` |  |
| `page.uncheck()` | `page.uncheck(selector).await` |  |
| `page.unroute()` | `page.unroute(url).await` |  |
| `page.unrouteAll()` | `page.unroute_all().await` |  |
| `page.url()` | `page.url()` |  |
| `page.video()` | `page.video()` |  |
| `page.viewportSize()` | `page.viewport_size()` |  |
| `page.waitForEvent()` | `page.wait_for_event(EventKind::Popup).await` | typed waiters: `wait_for_popup`, `wait_for_download`, `wait_for_dialog`, ...; `expect_*(action)` helpers |
| `page.waitForLoadState()` | `page.wait_for_load_state().await` |  |
| `page.waitForNavigation()` | `page.wait_for_navigation().await` |  |
| `page.waitForRequest()` | `page.wait_for_request(url).await` |  |
| `page.waitForResponse()` | `page.wait_for_response(url).await` |  |
| `page.waitForTimeout()` | `page.wait_for_timeout(ms).await` |  |
| `page.waitForURL()` | `page.wait_for_url(url).await` |  |
| `page.workers()` | `page.workers()` |  |
| `page.localStorage` | `page.local_storage()` | a `WebStorage`: `items`, `get_item`, `set_item`, `remove_item`, `clear`; runs in an isolated world |
| `page.sessionStorage` | `page.session_storage()` | same |
| `page.keyboard` | `page.keyboard()` |  |
| `page.mouse` | `page.mouse()` |  |
| `page.touchscreen` | `page.touchscreen()` |  |
| `page.clock` | `page.clock()` |  |
| `page.request` | `page.request()` |  |
| `page.coverage` | `page.start_js_coverage().await` | `stop_js_coverage`, `start_css_coverage`, `stop_css_coverage` are on the page; not stealthy |

Typed event handlers: `on_close`, `on_console`, `on_crash`, `on_dialog`, `on_dialog_closed`, `on_dom_content_loaded`, `on_download`, `on_file_chooser`, `on_frame_attached`, `on_frame_detached`, `on_frame_navigated`, `on_load`, `on_page_error`, `on_popup`, `on_request`, `on_request_failed`, `on_request_finished`, `on_response`, `on_websocket`, `on_worker`.

Also: `type_text`, `stop_js_coverage`, `start_css_coverage`, `stop_css_coverage`, `locator_with`, `get_by_alt_text_with`, `get_by_label_with`, `get_by_placeholder_with`, `get_by_text_with`, `get_by_title_with`, `get_by_role_with`, `frame_locator_any`, `cdp`, `frame_by_url`, `add_init_script_path`, `wait_for_popup`, `wait_for_download`, `wait_for_file_chooser`, `wait_for_dialog`, `wait_for_console_message`, `wait_for_page_error`, `wait_for_request_finished`, `wait_for_request_failed`, `wait_for_websocket`, `wait_for_worker`, `wait_for_close`, `wait_for_frame_event`, `expect_event`, `expect_popup`, `expect_download`, `expect_file_chooser`, `expect_console_message`, `expect_request`, `expect_response`, `expect_request_finished`, `expect_websocket`, `expect_worker`, `close_reason`.

### Frame

| Playwright | oxidewright | Notes |
|---|---|---|
| `frame.$()` | `frame.query_selector(selector).strict(b).await` |  |
| `frame.$$()` | `frame.query_selector_all(selector).await` |  |
| `frame.$eval()` | `frame.eval_on_selector(selector, expression).await` |  |
| `frame.$$eval()` | `frame.eval_on_selector_all(selector, expression).await` |  |
| `frame.evaluate()` | `frame.evaluate(expression).await` |  |
| `frame.evaluateHandle()` | `frame.evaluate_handle(expression).await` |  |
| `frame.waitForFunction()` | `frame.wait_for_function(expression).await` | see `page.waitForFunction()` |
| `frame.waitForSelector()` | `frame.wait_for_selector(selector).await` |  |
| `frame.addScriptTag()` | `frame.add_script_tag().await` |  |
| `frame.addStyleTag()` | `frame.add_style_tag().await` |  |
| `frame.check()` | `frame.check(selector).await` |  |
| `frame.childFrames()` | `frame.child_frames()` |  |
| `frame.click()` | `frame.click(selector).await` |  |
| `frame.content()` | `frame.content().include_shadow(b).await` |  |
| `frame.dblclick()` | `frame.dblclick(selector).await` |  |
| `frame.dispatchEvent()` | `frame.dispatch_event(selector, type_).await` |  |
| `frame.dragAndDrop()` | `frame.drag_and_drop(source, target).scroll(b).await` |  |
| `frame.fill()` | `frame.fill(selector, value).await` |  |
| `frame.focus()` | `frame.focus(selector).await` |  |
| `frame.frameElement()` | `frame.frame_element().await` |  |
| `frame.frameLocator()` | `frame.frame_locator(selector)` |  |
| `frame.getAttribute()` | `frame.get_attribute(selector, name).await` |  |
| `frame.getByAltText()` | `frame.get_by_alt_text(text)` |  |
| `frame.getByLabel()` | `frame.get_by_label(text)` |  |
| `frame.getByPlaceholder()` | `frame.get_by_placeholder(text)` |  |
| `frame.getByRole()` | `frame.get_by_role(role)` |  |
| `frame.getByTestId()` | `frame.get_by_test_id(test_id)` |  |
| `frame.getByText()` | `frame.get_by_text(text)` |  |
| `frame.getByTitle()` | `frame.get_by_title(text)` |  |
| `frame.goto()` | `frame.goto(url).await` |  |
| `frame.hover()` | `frame.hover(selector).await` |  |
| `frame.innerHTML()` | `frame.inner_html(selector).await` |  |
| `frame.innerText()` | `frame.inner_text(selector).await` |  |
| `frame.inputValue()` | `frame.input_value(selector).await` |  |
| `frame.isChecked()` | `frame.is_checked(selector).await` |  |
| `frame.isDetached()` | `frame.is_detached()` |  |
| `frame.isDisabled()` | `frame.is_disabled(selector).await` |  |
| `frame.isEditable()` | `frame.is_editable(selector).await` |  |
| `frame.isEnabled()` | `frame.is_enabled(selector).await` |  |
| `frame.isHidden()` | `frame.is_hidden(selector).await` |  |
| `frame.isVisible()` | `frame.is_visible(selector).await` |  |
| `frame.locator()` | `frame.locator(selector_or_locator)` |  |
| `frame.name()` | `frame.name()` |  |
| `frame.page()` | `frame.page()` |  |
| `frame.parentFrame()` | `frame.parent_frame()` |  |
| `frame.press()` | `frame.press(selector, key).await` |  |
| `frame.selectOption()` | `frame.select_option(selector, values).await` |  |
| `frame.setChecked()` | `frame.set_checked(selector, checked).await` |  |
| `frame.setContent()` | `frame.set_content(html).await` |  |
| `frame.setInputFiles()` | `frame.set_input_files(selector, files).await` |  |
| `frame.tap()` | `frame.tap(selector).await` |  |
| `frame.textContent()` | `frame.text_content(selector).await` |  |
| `frame.title()` | `frame.title().await` |  |
| `frame.type()` | `frame.r#type(selector, text).await` |  |
| `frame.uncheck()` | `frame.uncheck(selector).await` |  |
| `frame.url()` | `frame.url()` |  |
| `frame.waitForLoadState()` | `frame.wait_for_load_state().await` |  |
| `frame.waitForNavigation()` | `frame.wait_for_navigation().await` |  |
| `frame.waitForTimeout()` | `frame.wait_for_timeout(ms).await` |  |
| `frame.waitForURL()` | `frame.wait_for_url(url).await` |  |

Also: `type_text`, `id`, `has_load_state`, `main_context`, `utility_context`, `locator_with`, `get_by_alt_text_with`, `get_by_label_with`, `get_by_placeholder_with`, `get_by_text_with`, `get_by_title_with`, `get_by_role_with`, `frame_locator_any`.

### Locator

| Playwright | oxidewright | Notes |
|---|---|---|
| `locator.evaluate()` | `locator.evaluate::<T>(expression).arg(arg).await` |  |
| `locator.evaluateHandle()` | `locator.evaluate_handle(expression).await` |  |
| `locator.evaluateAll()` | `locator.evaluate_all(expression).await` |  |
| `locator.waitForFunction()` | `locator.wait_for_function("el => ..").arg(a).await` |  |
| `locator.elementHandle()` | `locator.element_handle().await` |  |
| `locator.highlight()` | `locator.highlight().style(css).await` | `style` as a CSS string |
| `locator.toString()` | `locator.selector()` | or `{locator:?}` |
| `locator.all()` | `locator.all().await` |  |
| `locator.allInnerTexts()` | `locator.all_inner_texts().await` |  |
| `locator.allTextContents()` | `locator.all_text_contents().await` |  |
| `locator.and()` | `locator.and_(locator)` |  |
| `locator.ariaSnapshot()` | `locator.aria_snapshot().mode(m).depth(n).boxes(b).await` |  |
| `locator.ariaSnapshotJSON()` | `locator.aria_snapshot_json().await` | a `serde_json::Value` |
| `locator.blur()` | `locator.blur().await` |  |
| `locator.boundingBox()` | `locator.bounding_box().await` |  |
| `locator.check()` | `locator.check().await` |  |
| `locator.clear()` | `locator.clear().await` |  |
| `locator.click()` | `locator.click().button(MouseButton::Right).timeout(5_000).await` |  |
| `locator.contentFrame()` | `locator.content_frame()` |  |
| `locator.count()` | `locator.count().await` |  |
| `locator.dblclick()` | `locator.dblclick().await` |  |
| `locator.describe()` | `locator.describe(description)` |  |
| `locator.description()` | `locator.description()` |  |
| `locator.dispatchEvent()` | `locator.dispatch_event(type_).await` |  |
| `locator.dragTo()` | `locator.drag_to(target).scroll(b).await` |  |
| `locator.drop()` | `locator.drop(DropPayload::new().files(..).data(mime, value)).await` |  |
| `locator.elementHandles()` | `locator.element_handles().await` |  |
| `locator.fill()` | `locator.fill(value).await` |  |
| `locator.filter()` | `locator.filter(LocatorOptions::new().has_text("x"))` |  |
| `locator.first()` | `locator.first()` |  |
| `locator.focus()` | `locator.focus().await` |  |
| `locator.frameLocator()` | `locator.frame_locator(selector)` |  |
| `locator.getAttribute()` | `locator.get_attribute(name).await` |  |
| `locator.getByAltText()` | `locator.get_by_alt_text(text)` |  |
| `locator.getByLabel()` | `locator.get_by_label(text)` |  |
| `locator.getByPlaceholder()` | `locator.get_by_placeholder(text)` |  |
| `locator.getByRole()` | `locator.get_by_role(role)` |  |
| `locator.getByTestId()` | `locator.get_by_test_id(test_id)` |  |
| `locator.getByText()` | `locator.get_by_text(text)` |  |
| `locator.getByTitle()` | `locator.get_by_title(text)` |  |
| `locator.hideHighlight()` | `locator.hide_highlight().await` |  |
| `locator.hover()` | `locator.hover().await` |  |
| `locator.innerHTML()` | `locator.inner_html().await` |  |
| `locator.innerText()` | `locator.inner_text().await` |  |
| `locator.inputValue()` | `locator.input_value().await` |  |
| `locator.isChecked()` | `locator.is_checked().await` |  |
| `locator.isDisabled()` | `locator.is_disabled().await` |  |
| `locator.isEditable()` | `locator.is_editable().await` |  |
| `locator.isEnabled()` | `locator.is_enabled().await` |  |
| `locator.isHidden()` | `locator.is_hidden().await` |  |
| `locator.isVisible()` | `locator.is_visible().await` |  |
| `locator.last()` | `locator.last()` |  |
| `locator.locator()` | `locator.locator(selector_or_locator)` |  |
| `locator.normalize()` | `locator.normalize().await` |  |
| `locator.nth()` | `locator.nth(index)` |  |
| `locator.or()` | `locator.or_(locator)` |  |
| `locator.page()` | `locator.page()` |  |
| `locator.press()` | `locator.press(key).await` |  |
| `locator.pressSequentially()` | `locator.press_sequentially(text).await` |  |
| `locator.screenshot()` | `locator.screenshot().await` |  |
| `locator.scrollIntoViewIfNeeded()` | `locator.scroll_into_view_if_needed().await` |  |
| `locator.selectOption()` | `locator.select_option(values).await` |  |
| `locator.selectText()` | `locator.select_text().await` |  |
| `locator.setChecked()` | `locator.set_checked(checked).await` |  |
| `locator.setInputFiles()` | `locator.set_input_files(files).await` |  |
| `locator.tap()` | `locator.tap().await` |  |
| `locator.textContent()` | `locator.text_content().await` |  |
| `locator.type()` | `locator.r#type(text).await` | deprecated in Playwright; prefer `press_sequentially` |
| `locator.uncheck()` | `locator.uncheck().await` |  |
| `locator.visible()` | `locator.visible()` |  |
| `locator.waitFor()` | `locator.wait_for().state(WaitForSelectorState::Hidden).await` |  |
| `locator.within()` | `locator.within(locator)` |  |

Also: `frame`, `locator_with`, `get_by_alt_text_with`, `get_by_label_with`, `get_by_placeholder_with`, `get_by_text_with`, `get_by_title_with`, `get_by_role_with`.

### FrameLocator

| Playwright | oxidewright | Notes |
|---|---|---|
| `frameLocator.first()` | `frame_locator.first()` |  |
| `frameLocator.frameLocator()` | `frame_locator.frame_locator(selector)` |  |
| `frameLocator.getByAltText()` | `frame_locator.get_by_alt_text(text)` |  |
| `frameLocator.getByLabel()` | `frame_locator.get_by_label(text)` |  |
| `frameLocator.getByPlaceholder()` | `frame_locator.get_by_placeholder(text)` |  |
| `frameLocator.getByRole()` | `frame_locator.get_by_role(role)` |  |
| `frameLocator.getByTestId()` | `frame_locator.get_by_test_id(test_id)` |  |
| `frameLocator.getByText()` | `frame_locator.get_by_text(text)` |  |
| `frameLocator.getByTitle()` | `frame_locator.get_by_title(text)` |  |
| `frameLocator.last()` | `frame_locator.last()` |  |
| `frameLocator.locator()` | `frame_locator.locator(selector_or_locator)` |  |
| `frameLocator.nth()` | `frame_locator.nth(index)` |  |
| `frameLocator.owner()` | `frame_locator.owner()` |  |

Also: `locator_with`, `get_by_alt_text_with`, `get_by_label_with`, `get_by_placeholder_with`, `get_by_text_with`, `get_by_title_with`, `get_by_role_with`.

### ElementHandle

| Playwright | oxidewright | Notes |
|---|---|---|
| `handle.$()` | `handle.query_selector(selector).await` |  |
| `handle.$$()` | `handle.query_selector_all(selector).await` |  |
| `handle.$eval()` | `handle.eval_on_selector(selector, expression).await` |  |
| `handle.$$eval()` | `handle.eval_on_selector_all(selector, expression).await` |  |
| `handle.waitForSelector()` | `handle.wait_for_selector(selector).await` |  |
| `handle.boundingBox()` | `handle.bounding_box().await` |  |
| `handle.check()` | `handle.check().await` |  |
| `handle.click()` | `handle.click().await` |  |
| `handle.contentFrame()` | `handle.content_frame().await` |  |
| `handle.dblclick()` | `handle.dblclick().await` |  |
| `handle.dispatchEvent()` | `handle.dispatch_event(type_).await` |  |
| `handle.fill()` | `handle.fill(value).await` |  |
| `handle.focus()` | `handle.focus().await` |  |
| `handle.getAttribute()` | `handle.get_attribute(name).await` |  |
| `handle.hover()` | `handle.hover().await` |  |
| `handle.innerHTML()` | `handle.inner_html().await` |  |
| `handle.innerText()` | `handle.inner_text().await` |  |
| `handle.inputValue()` | `handle.input_value().await` |  |
| `handle.isChecked()` | `handle.is_checked().await` |  |
| `handle.isDisabled()` | `handle.is_disabled().await` |  |
| `handle.isEditable()` | `handle.is_editable().await` |  |
| `handle.isEnabled()` | `handle.is_enabled().await` |  |
| `handle.isHidden()` | `handle.is_hidden().await` |  |
| `handle.isVisible()` | `handle.is_visible().await` |  |
| `handle.ownerFrame()` | `handle.owner_frame().await` |  |
| `handle.press()` | `handle.press(key).await` |  |
| `handle.screenshot()` | `handle.screenshot().await` |  |
| `handle.scrollIntoViewIfNeeded()` | `handle.scroll_into_view_if_needed().await` |  |
| `handle.selectOption()` | `handle.select_option(values).await` |  |
| `handle.selectText()` | `handle.select_text().await` |  |
| `handle.setChecked()` | `handle.set_checked(checked).await` |  |
| `handle.setInputFiles()` | `handle.set_input_files(files).await` |  |
| `handle.tap()` | `handle.tap().await` |  |
| `handle.textContent()` | `handle.text_content().await` |  |
| `handle.type()` | `handle.r#type(text).await` |  |
| `handle.uncheck()` | `handle.uncheck().await` |  |
| `handle.waitForElementState()` | `handle.wait_for_element_state(state).await` |  |

Also: `type_text`, `as_js_handle`, `same_object`, `evaluate`, `evaluate_handle`, `get_property`, `get_properties`, `json_value`, `dispose`, `as_element`, `is_element`.

### JSHandle

| Playwright | oxidewright | Notes |
|---|---|---|
| `handle.evaluate()` | `handle.evaluate(expression).await` |  |
| `handle.evaluateHandle()` | `handle.evaluate_handle(expression).await` |  |
| `handle.jsonValue()` | `handle.json_value().await` |  |
| `handle.asElement()` | `handle.as_element()` |  |
| `handle.dispose()` | `handle.dispose().await` |  |
| `handle.getProperties()` | `handle.get_properties().await` |  |
| `handle.getProperty()` | `handle.get_property(name).await` |  |

Also: `same_object`, `is_element`.

### Keyboard

| Playwright | oxidewright | Notes |
|---|---|---|
| `keyboard.down()` | `keyboard.down(key).await` |  |
| `keyboard.insertText()` | `keyboard.insert_text(text).await` |  |
| `keyboard.press()` | `keyboard.press(key).await` |  |
| `keyboard.type()` | `keyboard.r#type(text).delay(50).await` | alias `type_text` |
| `keyboard.up()` | `keyboard.up(key).await` |  |

Also: `type_text`.

### Mouse

| Playwright | oxidewright | Notes |
|---|---|---|
| `mouse.click()` | `mouse.click(x, y).await` |  |
| `mouse.dblclick()` | `mouse.dblclick(x, y).await` |  |
| `mouse.down()` | `mouse.down().await` |  |
| `mouse.move()` | `mouse.r#move(x, y).steps(5).await` | alias `move_to` |
| `mouse.up()` | `mouse.up().await` |  |
| `mouse.wheel()` | `mouse.wheel(delta_x, delta_y).await` |  |

Also: `move_to`.

### Touchscreen

| Playwright | oxidewright | Notes |
|---|---|---|
| `touchscreen.tap()` | `touchscreen.tap(x, y).await` |  |

### Request

| Playwright | oxidewright | Notes |
|---|---|---|
| `request.allHeaders()` | `request.all_headers().await` |  |
| `request.existingResponse()` | `request.existing_response()` |  |
| `request.failure()` | `request.failure()` |  |
| `request.frame()` | `request.frame()` |  |
| `request.headers()` | `request.headers()` |  |
| `request.headersArray()` | `request.headers_array().await` |  |
| `request.headerValue()` | `request.header_value(name).await` |  |
| `request.isNavigationRequest()` | `request.is_navigation_request()` |  |
| `request.method()` | `request.method()` |  |
| `request.postData()` | `request.post_data()` |  |
| `request.postDataBuffer()` | `request.post_data_buffer()` |  |
| `request.postDataJSON()` | `request.post_data_json().await` |  |
| `request.redirectedFrom()` | `request.redirected_from()` |  |
| `request.redirectedTo()` | `request.redirected_to()` |  |
| `request.resourceType()` | `request.resource_type()` |  |
| `request.response()` | `request.response().await` |  |
| `request.serviceWorker()` | missing | network events of service worker sessions are not tracked |
| `request.sizes()` | `request.sizes().await` |  |
| `request.timing()` | `request.timing()` |  |
| `request.url()` | `request.url()` |  |

Also: `page`, `is_service_worker_request`.

### Response

| Playwright | oxidewright | Notes |
|---|---|---|
| `response.allHeaders()` | `response.all_headers().await` |  |
| `response.body()` | `response.body().await` |  |
| `response.finished()` | `response.finished().await` |  |
| `response.frame()` | `response.frame()` |  |
| `response.fromServiceWorker()` | `response.from_service_worker()` |  |
| `response.headers()` | `response.headers()` |  |
| `response.headersArray()` | `response.headers_array().await` |  |
| `response.headerValue()` | `response.header_value(name).await` |  |
| `response.headerValues()` | `response.header_values(name).await` |  |
| `response.httpVersion()` | `response.http_version().await` |  |
| `response.json()` | `response.json().await` |  |
| `response.ok()` | `response.ok()` |  |
| `response.request()` | `response.request()` |  |
| `response.securityDetails()` | `response.security_details().await` |  |
| `response.serverAddr()` | `response.server_addr().await` |  |
| `response.status()` | `response.status()` |  |
| `response.statusText()` | `response.status_text()` |  |
| `response.text()` | `response.text().await` |  |
| `response.url()` | `response.url()` |  |

Also: `from_cache`, `sizes`.

### Route

| Playwright | oxidewright | Notes |
|---|---|---|
| `route.abort()` | `route.abort().error_code("failed").await` |  |
| `route.continue()` | `route.continue_().headers(headers).await` |  |
| `route.fallback()` | `route.fallback().url(url).await` |  |
| `route.fetch()` | `route.fetch().timeout(ms).max_redirects(n).max_retries(n).await` | first hop goes through the browser; redirects (default 20) are followed with the context's `APIRequestContext`; `max_retries` applies to those |
| `route.fulfill()` | `route.fulfill().status(200).json(&value).await` | also `body`, `path`, `headers`, `content_type`, `response` |
| `route.request()` | `route.request()` |  |

### APIRequest

| Playwright | oxidewright | Notes |
|---|---|---|
| `request.newContext()` | `oxidewright::request().new_context().base_url(url).await` |  |
| `request.newContext({ clientCertificates })` | `.client_certificates([ClientCertificate::new(origin).cert_path(..).key_path(..)])` | PEM cert + key per origin; `pfx` and encrypted keys not yet; browser contexts do not take client certificates yet |

### APIRequestContext

| Playwright | oxidewright | Notes |
|---|---|---|
| `api.addCookies()` | `api.add_cookies(cookies).await` |  |
| `api.clearCookies()` | `api.clear_cookies().await` |  |
| `api.cookies()` | `api.cookies().await` |  |
| `api.delete()` | `api.delete(url).await` |  |
| `api.dispose()` | `api.dispose().await` |  |
| `api.fetch()` | `api.fetch(url).await` |  |
| `api.get()` | `api.get(url).await` |  |
| `api.head()` | `api.head(url).await` |  |
| `api.patch()` | `api.patch(url).await` |  |
| `api.post()` | `api.post(url).json(&value).header("x", "y").await` |  |
| `api.put()` | `api.put(url).await` |  |
| `api.storageState()` | `api.storage_state().await` |  |

### APIResponse (`oxidewright::APIResponse`)

| Playwright | oxidewright | Notes |
|---|---|---|
| `response.json()` | `response.json().await` |  |
| `response.body()` | `response.body().await` |  |
| `response.dispose()` | `response.dispose().await` |  |
| `response.headers()` | `response.headers()` |  |
| `response.headersArray()` | `response.headers_array()` |  |
| `response.ok()` | `response.ok()` |  |
| `response.securityDetails()` | `response.security_details().await` | subject, issuer and validity; `protocol` is `None` (not reported by the HTTP client) |
| `response.serverAddr()` | `response.server_addr().await` |  |
| `response.status()` | `response.status()` |  |
| `response.statusText()` | `response.status_text()` |  |
| `response.text()` | `response.text().await` |  |
| `response.timing()` | `response.timing()` | DNS/connect/TLS phases are -1 (not reported by the HTTP client) |
| `response.url()` | `response.url()` |  |

Also: `header_value`, `header_values`.

### Dialog

| Playwright | oxidewright | Notes |
|---|---|---|
| `dialog.accept()` | `dialog.accept(Some("text")).await` |  |
| `dialog.defaultValue()` | `dialog.default_value()` |  |
| `dialog.dismiss()` | `dialog.dismiss().await` |  |
| `dialog.message()` | `dialog.message()` |  |
| `dialog.page()` | `dialog.page()` |  |
| `dialog.type()` | `dialog.r#type()` |  |

### Download

| Playwright | oxidewright | Notes |
|---|---|---|
| `download.cancel()` | `download.cancel().await` |  |
| `download.createReadStream()` | missing | use `path()` or `save_as()` |
| `download.delete()` | `download.delete().await` |  |
| `download.failure()` | `download.failure().await` |  |
| `download.page()` | `download.page()` |  |
| `download.path()` | `download.path().await` |  |
| `download.saveAs()` | `download.save_as(path).await` |  |
| `download.suggestedFilename()` | `download.suggested_filename()` |  |
| `download.url()` | `download.url()` |  |

### FileChooser

| Playwright | oxidewright | Notes |
|---|---|---|
| `chooser.element()` | `chooser.element()` |  |
| `chooser.isMultiple()` | `chooser.is_multiple()` |  |
| `chooser.page()` | `chooser.page()` |  |
| `chooser.setFiles()` | `chooser.set_files(files).timeout(ms).no_wait_after(b).await` | paths or `FilePayload`s |

### ConsoleMessage

| Playwright | oxidewright | Notes |
|---|---|---|
| `message.args()` | `message.args()` |  |
| `message.location()` | `message.location()` |  |
| `message.page()` | `message.page()` |  |
| `message.text()` | `message.text()` |  |
| `message.timestamp()` | `message.timestamp()` |  |
| `message.type()` | `message.r#type()` |  |
| `message.worker()` | `message.worker()` |  |

### WebSocket

| Playwright | oxidewright | Notes |
|---|---|---|
| `ws.on()` | `ws.on(\|event\| ..)` | or `on_frame_sent`, `on_frame_received`, `on_close`, `on_socket_error` |
| `ws.once()` | `ws.on(\|event\| ..)` |  |
| `ws.removeListener()` | missing | handlers stay until the socket closes |
| `ws.isClosed()` | `ws.is_closed()` |  |
| `ws.url()` | `ws.url()` |  |
| `ws.waitForEvent()` | `ws.wait_for_event(\|event\| .., timeout_ms).await` |  |

Typed event handlers: `on_frame_sent`, `on_frame_received`, `on_close`, `on_socket_error`.

### Worker

| Playwright | oxidewright | Notes |
|---|---|---|
| `worker.evaluate()` | `worker.evaluate::<T>(expression).arg(arg).await` |  |
| `worker.evaluateHandle()` | `worker.evaluate_handle(expression).await` |  |
| `worker.on()` | `worker.on_close(\|\| ..)`, `worker.on_console(\|m\| ..)` | `console` needs `console_events(true)` |
| `worker.once()` | `worker.on_close(\|\| ..)` | only `close` |
| `worker.removeListener()` | missing | handlers stay until the worker closes |
| `worker.url()` | `worker.url()` |  |
| `worker.waitForEvent()` | `worker.wait_for_close().await` | only `close` |

Typed event handlers: `on_close`.

Also: `page`, `is_closed`.

### Tracing

| Playwright | oxidewright | Notes |
|---|---|---|
| `tracing.group()` | `tracing.group(name).await` |  |
| `tracing.groupEnd()` | `tracing.group_end().await` |  |
| `tracing.start()` | `tracing.start().screenshots(true).await` |  |
| `tracing.startChunk()` | `tracing.start_chunk().await` |  |
| `tracing.startHar()` | `tracing.start_har(path).mode(HarMode::Minimal).url_filter(glob).await` | `content`, `mode`, `url_filter`; no `resourcesDir`, no `.zip` |
| `tracing.stop()` | `tracing.stop().path(path).await` |  |
| `tracing.stopChunk()` | `tracing.stop_chunk().path(path).await` |  |
| `tracing.stopHar()` | `tracing.stop_har().await` |  |

### Clock

| Playwright | oxidewright | Notes |
|---|---|---|
| `clock.fastForward()` | `clock.fast_forward(ticks).await` |  |
| `clock.install()` | `clock.install().time(time).await` |  |
| `clock.pauseAt()` | `clock.pause_at(time).await` |  |
| `clock.resume()` | `clock.resume().await` |  |
| `clock.runFor()` | `clock.run_for(ticks).await` |  |
| `clock.setFixedTime()` | `clock.set_fixed_time(time).await` |  |
| `clock.setSystemTime()` | `clock.set_system_time(time).await` |  |

### CDPSession

| Playwright | oxidewright | Notes |
|---|---|---|
| `session.on()` | `session.event_listener::<EventType>().await` | a typed stream of chromiumoxide CDP events |
| `session.on('event', ...)` | `session.all_events().await` | every event as `CdpEvent { method, params }` |
| `session.removeListener()` | missing | drop the stream |
| `session.once()` | `session.event_listener::<EventType>().await` | take one item from the stream |
| `session.send()` | `session.send("Method.name", json!({..})).timeout(ms).await` | returns `serde_json::Value` |
| `session.detach()` | `session.detach().await` |  |
| `session.on('close')` | `session.on_close(\|\| ..)` / `session.wait_for_close().await` | detach, page close or frame detach |
| `session.on('event')` | missing | chromiumoxide only dispatches typed per-method events |

### Video

| Playwright | oxidewright | Notes |
|---|---|---|
| `video.delete()` | `video.delete().await` |  |
| `video.path()` | `video.path()` |  |
| `video.saveAs()` | `video.save_as(path).await` |  |

### Screencast

| Playwright | oxidewright | Notes |
|---|---|---|
| `page.screencast.start({ onFrame, path, size, quality, fps })` | `page.screencast().start().on_frame(\|f\| ..).path(p).size(..).quality(..).fps(..).await` | frames are JPEG (`ScreencastFrame`); `path` records a webm video (needs ffmpeg) |
| `page.screencast.stop()` | `page.screencast().stop().await` | finalizes the video |
| `showActions` / `hideActions` / `showOverlay(s)` / `hideOverlays` / `showChapter` | not implemented | they draw into the page, which page scripts can observe |

### Selectors

| Playwright | oxidewright | Notes |
|---|---|---|
| `selectors.register(name, script, { contentScript })` | `selectors().register(name, script).content_script(true)`; `register_path(name, path)` | process-wide; engines without `content_script` run in the page's main world (visible to the page) |
| `selectors.setTestIdAttribute()` | `selectors().set_test_id_attribute(attribute_name)` |  |


`route.fetch()` returns the same `oxidewright::APIResponse` as `APIRequestContext` requests
(`APIResponse::new(status, headers, body)` builds one to fulfill a route with).

### WebSocketRoute

| Playwright | oxidewright | Notes |
|---|---|---|
| `ws.close()` | `ws.close().await` / `ws.close_with(Some(code), Some(reason)).await` |  |
| `ws.connectToServer()` | `ws.connect_to_server()?` | returns `WebSocketRouteServer` (`send`, `on_message`, `on_close`, `close`, `url`, `protocols`) |
| `ws.onClose()` | `ws.on_close(\|code, reason\| ..)` |  |
| `ws.onMessage()` | `ws.on_message(\|message\| ..)` | `WebSocketPayload::Text`/`Binary` |
| `ws.protocols()` | `ws.protocols()` |  |
| `ws.send()` | `ws.send(message)` |  |
| `ws.url()` | `ws.url()` |  |

### Credentials

| Playwright | oxidewright | Notes |
|---|---|---|
| `credentials.create()` | `credentials.create(rp_id).sign_count(n).await` | or `.from(&credential)` to import one |
| `credentials.delete()` | `credentials.delete(id).await` |  |
| `credentials.get()` | `credentials.get().rp_id(rp).id(id).await` | includes credentials registered by pages and current sign counters |
| `credentials.install()` | `credentials.install().await` | one CDP virtual authenticator per page; `navigator.credentials` stays native |

### Not ported as classes

* `Coverage`: `page.start_js_coverage()`, `stop_js_coverage()`, `start_css_coverage()`,
  `stop_css_coverage()` on `Page` (not stealthy: they enable the `Debugger`/`Profiler` or
  `CSS` domains).
* `WebError`: the `pageerror`/`weberror` payload is `oxidewright::WebError` with `name`,
  `message` and `stack` fields (`webError.error()`), plus `location()` and `page()`.
* `Electron`, `Android`, `firefox`, `webkit`: Chromium only.
* `page.pause()`, `pickLocator()`, `context.debugger` (`Debugger`): they drive Playwright's
  inspector UI, which only exists in the Node.js package.
* `WebMCP` (`page.webmcp`): relies on an experimental Chrome API for AI agents that stable
  Chrome does not expose.
* `browserType.launchServer()` / `connect()` / `BrowserServer`: the Playwright server protocol
  (the Node.js server); use `connect_over_cdp` to share a browser.
* `clientCertificates` of browser contexts: Playwright proxies the pages' TLS through an
  internal interceptor; only `APIRequestContext` takes client certificates for now.
* `exposeFunctions` (option of `evaluate`/`addInitScript`): JavaScript functions inside the
  argument become callable in the page. Rust arguments are data and cannot carry closures; use
  `page.expose_function` / `expose_binding` (which also avoid the bindings-controller global the
  option installs in the page's main world).

## Assertions (expect)

`oxidewright::expect` has Playwright Test's web-first assertions. They retry until the
timeout (5 s by default, `expect::set_default_timeout`) and fail with `Error::Expect`, whose
`MatcherResult` carries Playwright's message, the call log and the expected/received values.

| Playwright | oxidewright |
|---|---|
| `await expect(locator).toBeVisible()` | `expect(&locator).to_be_visible().await?` |
| `await expect(locator).not.toHaveText('x', { timeout: 1000 })` | `expect(&locator).not().to_have_text("x").timeout(1000).await?` |
| `await expect(locator, 'msg').toBeChecked({ checked: false })` | `expect(&locator).message("msg").to_be_checked().checked(false).await?` |
| `await expect(locator).toHaveText(['a', /b/])` | `expect(&locator).to_have_text(vec![TextMatch::from("a"), regex("b", "").into()]).await?` |
| `await expect(locator).toContainText('x', { ignoreCase: true, useInnerText: true })` | `expect(&locator).to_contain_text("x").ignore_case(true).use_inner_text(true).await?` |
| `await expect(locator).toHaveAttribute('id')` | `expect(&locator).to_have_attribute_present("id").await?` |
| `await expect(locator).toHaveAttribute('id', 'a')` | `expect(&locator).to_have_attribute("id", "a").await?` |
| `await expect(locator).toHaveJSProperty('a.b', 1)` | `expect(&locator).to_have_js_property("a.b", 1).await?` |
| `await expect(locator).toBeInViewport({ ratio: 0.5 })` | `expect(&locator).to_be_in_viewport().ratio(0.5).await?` |
| `await expect(locator).toHaveCSS('color', 'red', { pseudo: 'before' })` | `expect(&locator).to_have_css("color", "red").pseudo("before").await?` |
| `await expect(locator).toMatchAriaSnapshot(yaml)` | `expect(&locator).to_match_aria_snapshot(yaml).await?` |
| `await expect(page).toHaveURL(/done/)` | `expect(&page).to_have_url(regex("done", "")).await?` |
| `await expect(page).toHaveURL(url => ...)` | `expect(&page).to_have_url_matching(\|url\| ...).await?` |
| `await expect(page).toHaveTitle('x')` | `expect(&page).to_have_title("x").await?` |
| `await expect(page).toHaveScreenshot('a.png')` | `expect(&page).to_have_screenshot("path/a.png").await?` |
| `await expect(response).toBeOK()` | `expect(&response).to_be_ok().await?` |
| `expect.configure({ timeout: 10000 })` | `expect(&x).timeout(10_000)` or `expect::set_default_timeout(10_000)` |

Also: `to_be_attached`, `to_be_disabled`, `to_be_editable`, `to_be_empty`, `to_be_enabled`,
`to_be_focused`, `to_be_hidden`, `to_have_accessible_name`, `to_have_accessible_description`,
`to_have_accessible_error_message`, `to_have_class`, `to_contain_class`, `to_have_count`,
`to_have_id`, `to_have_role`, `to_have_value`, `to_have_values`.

`expect.poll(fn)` is `expect::poll(fn)` and `expect(fn).toPass()` is `expect::to_pass(fn)`.

Not ported: soft assertions, `expect.extend`, the `signal` option
(drop the future instead), `URLPattern`, snapshot path templates and `--update-snapshots`
(`to_have_screenshot` takes an explicit PNG path: a missing file is written and the assertion
fails).
