# oxidewright

A Rust port of the [Playwright](https://playwright.dev) API for Chromium, built on top of
[chromiumoxide](../docs/protocol-layer.md), with [Patchright](https://github.com/Kaliiiiiiiiii-Vinyzu/patchright)-level
stealth: the protocol traces that page scripts can observe are never left behind.

> **Disclaimer.** oxidewright is an independent project. It is inspired by Playwright and
> Patchright and bundles their injected scripts (Apache-2.0, see [NOTICE](NOTICE)), but it is
> **not affiliated with, endorsed by or sponsored by Microsoft** or the Playwright/Patchright
> projects.

## Install

oxidewright is not published on crates.io. Depend on it from git:

```toml
[dependencies]
oxidewright = { git = "https://github.com/hugocorreia2004/oxidewright" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

* MSRV: Rust 1.85 (edition 2024). Async runtime: tokio.
* A Chrome or Chromium binary must be installed (it is detected, or pass
  `.executable_path(..)` to `launch()`).
* Cargo feature `stealth` (on by default) builds chromiumoxide with its own `stealth` feature.

## Quick start

```rust,no_run
use oxidewright::{chromium, LoadState};

#[tokio::main]
async fn main() -> oxidewright::Result<()> {
    let browser = chromium().launch().headless(true).await?;
    let context = browser.new_context().await?;
    let page = context.new_page().await?;

    let response = page.goto("https://example.com").await?;
    println!("status: {:?}", response.map(|r| r.status()));

    // Locators auto-wait and are strict, like Playwright's.
    page.get_by_role("link").first().click().timeout(5_000).await?;

    // Expressions or functions, called with the argument:
    let title: String = page.evaluate("() => document.title").await?;
    let sum: i32 = page.evaluate("([a, b]) => a + b").arg((1, 2)).await?;
    println!("{title} {sum}");

    page.wait_for_load_state().state(LoadState::NetworkIdle).await?;
    page.screenshot().path("page.png").full_page(true).await?;
    browser.close().await?;
    Ok(())
}
```

Coming from Playwright JS/TS? See [docs/from-playwright.md](docs/from-playwright.md) for the
conventions and a method-by-method mapping of every class.

## Examples

Runnable examples live in [`examples/`](examples). Except `connect_over_cdp`, they serve their
pages from routes on a made-up origin, so they need no network:

| Example | Shows |
|---|---|
| [`screenshot`](examples/screenshot.rs) | launch, viewport, page/element/full-page screenshots |
| [`connect_over_cdp`](examples/connect_over_cdp.rs) | attach to a Chrome started with `--remote-debugging-port` |
| [`locators_forms`](examples/locators_forms.rs) | `get_by_*` locators, filters, filling a form |
| [`network_mocking`](examples/network_mocking.rs) | `route` with `fulfill`/`abort`/`fallback`, request events |
| [`wait_for_events`](examples/wait_for_events.rs) | `wait_for_response`, `expect_popup`, dialogs, timeouts |
| [`storage_state`](examples/storage_state.rs) | save and restore cookies and `localStorage` |
| [`har`](examples/har.rs) | record a HAR and replay it with `route_from_har` |
| [`tracing`](examples/tracing.rs) | record a trace for `npx playwright show-trace` |

```sh
cargo run -p oxidewright --example locators_forms
```

## Conventions

* Same classes and methods as Playwright, in `snake_case`: `page.wait_for_load_state()`,
  `locator.press_sequentially(..)`, `handle.json_value()`.
* Option objects become builder methods chained before `.await` (the builders implement
  `IntoFuture`): `page.goto(url).wait_until(LoadState::Commit).timeout(5_000).await` is
  `await page.goto(url, { waitUntil: 'commit', timeout: 5000 })`. Option names and defaults
  are Playwright's.
* Reserved words: `r#type` (alias `type_text`), `r#move` (alias `move_to`), `continue_`,
  `and_`, `or_`.
* `evaluate` takes the JavaScript source as a string; if it is a function it is called with
  `.arg(..)` (any `serde::Serialize` value, may contain `JSHandle`s). Results deserialize into
  any `DeserializeOwned` type, through Playwright's own serializer, so `undefined`, `NaN`, `-0`,
  `Date`, `RegExp`, `BigInt`, `Map`/`Set` and handles round-trip; use `oxidewright::JsValue` to
  keep those distinctions.
* Errors are the `oxidewright::Error` enum (`Timeout`, `TargetClosed`, `Navigation`,
  `Evaluation`, `StrictModeViolation`, ...). Messages mirror Playwright's, e.g.
  `locator.click: Timeout 30000ms exceeded.` followed by a call log.
* Events: closures (`page.on_request(|req| ..)`, `page.on(EventKind::Popup, ..)`), waiters
  that subscribe when called (`page.wait_for_response("**/api")`) and `expect_*(action)`
  helpers (`page.expect_popup(action)`).
* `Browser`, `BrowserContext`, `Page`, `Frame`, `Locator` and handles are cheap `Clone` values.

## Stealth

What oxidewright never sends:

* `Runtime.enable`, `Log.enable`, `Performance.enable` (also not by chromiumoxide with the
  `stealth` feature). `Runtime.addBinding` is sent only by `expose_function`/`expose_binding`
  (with the user's name, like Patchright) and by `route_web_socket` (`__pwWebSocketBinding`).
* No utility world created at attach. Execution contexts are created on demand with
  `Page.createIsolatedWorld` (random name), cached per frame and dropped on navigation or
  detach. The selector engine and actions run in that isolated world, which shares the DOM
  with the page but not its JavaScript globals.
* Main-world evaluation (`page.evaluate`) goes through the `globalThis` object of the page
  (Patchright's technique), never through context ids learned from `Runtime` events. Page
  scripts can observe what your code does there, not how it got there.
* `Fetch.enable` is only sent while routes (or HTTP credentials) exist;
  `Page.setInterceptFileChooserDialog` only while a file chooser is awaited or listened for.
* Tracing and video use `Page.startScreencast`, which pages cannot observe. Screenshots never
  bring the page to front.

Opt-in features that **are** detectable when you enable them:

* `console` / `pageerror` events need `Runtime.enable`: create the context with
  `.console_events(true)` to get them.
* JS coverage enables the `Debugger` and `Profiler` domains (CSS coverage `DOM`/`CSS`).
* Every emulation override (viewport, user agent, locale, timezone, media features,
  geolocation, ...) changes what the page sees, so it is only sent when the option is set.
  Contexts from `browser.new_context()` keep Playwright's 1280x720 default viewport (use
  `.viewport(None)` to keep the window size); the default context of `connect_over_cdp`
  overrides nothing; unset media features are not forced to `light`/`no-preference`.
* CDP sessions (`new_cdp_session`) share the page session: whatever you send is your call.
* `route_web_socket` injects Playwright's WebSocket mock script into the page and adds the
  `__pwWebSocketBinding` binding, like Playwright: pages can detect it while it is in use.

## Feature matrix

| Area | Status |
|---|---|
| `chromium().launch()` / `connect_over_cdp()` | Implemented, with `launch_persistent_context`. Missing: `connect`, `launch_server` |
| Browser, BrowserContext, Page, Frame | Implemented (navigation, lifecycle incl. `networkidle`, frames incl. out-of-process iframes, init scripts, tags, `close` with reason) |
| Locators, `get_by_*`, FrameLocator, filters, strict mode | Implemented |
| Actions (click, fill, press, check, select, upload, drag, ...) with actionability checks | Implemented |
| ElementHandle / JSHandle | Implemented |
| Keyboard, Mouse, Touchscreen | Implemented |
| Network: Request, Response, `route`, `unroute`, `route.fetch` | Implemented (`route.fetch` goes through the browser network stack) |
| Events and `wait_for_*` / `expect_*` | Implemented: popup, page, close, crash, dialog, download, filechooser, load, domcontentloaded, frame events, request events, websocket, worker; console/pageerror opt-in |
| Emulation and context options, `devices()` | Implemented (out-of-process iframes are not emulated separately yet) |
| Cookies, storage state | Implemented (Playwright-compatible JSON) |
| Screenshots, PDF | Implemented |
| HAR: `record_har`, `route_from_har` | Implemented; zipped HAR files not supported |
| APIRequestContext (`request()`, `context.request()`) | Implemented, incl. `timing()`, `server_addr()`, `security_details()` and `client_certificates` (PEM only); requests use rustls, not Chrome's fingerprint |
| Tracing | Partial: screencast frames and groups; no per-action events, DOM snapshots, network or console entries |
| Video | Implemented with an external `ffmpeg` (`OXIDEWRIGHT_FFMPEG_PATH` or `PATH`) |
| Clock, coverage, CDP sessions, selectors (`register`, `set_test_id_attribute`) | Implemented |
| Workers, service workers | Implemented |
| Assertions (`expect`) | Implemented: locator, page and API response matchers incl. `to_match_aria_snapshot` and `to_have_screenshot`, `expect::poll` and `expect::to_pass`; no soft assertions or `expect.extend` (see [Assertions](#assertions-expect)) |
| Also implemented | Human-like input ([`oxidewright::human`](#human-like-input-oxidewrighthuman)), `wait_for_function`, `wait_for_timeout`, `expose_function`/`expose_binding`, `add_locator_handler`, `route_web_socket`, `aria_snapshot`, `get_by_ref`, `page.screencast()`, `browser.start_tracing` |
| Missing | `pause`/inspector, WebMCP, `launch_server`/`connect`, client certificates of browser contexts, the `exposeFunctions` option, routes on the very first request of a `noopener` popup or of an out-of-process iframe (see [docs/from-playwright.md](docs/from-playwright.md#not-ported-as-classes)) |

## Human-like input (`oxidewright::human`)

Not part of Playwright: a `Human` is one simulated person, with traits drawn
once (speed, typing rhythm, key hold times, hand tremor, fatigue, scroll
style), whose mouse, keyboard, pauses and scrolling look like a person's.

```rust,no_run
# async fn run(page: oxidewright::Page) -> oxidewright::Result<()> {
use oxidewright::human::{Human, KeyboardLayout, PauseKind, ReadingKind};

// The person of this browser context: created on first use, the same one
// after that, dropped when the context (or the browser) closes.
page.context().unwrap().set_human(Human::new().layout(KeyboardLayout::Es));
let human = page.human();
human.read(&page, ReadingKind::ScanningPage).await;
human.type_text(&page.locator("#name"), "María").await?;
human.type_text(&page.locator("#dob"), "14031987").groups([2, 2, 4]).await?;
human.type_verified(&page.locator("#password"), "s3cret!").await?;
human.click(&page.locator("button[type=submit]")).await?;
human.pause(PauseKind::AfterSubmit).await;
# Ok(()) }
```

| What | How |
|---|---|
| Mouse | Curved paths with Perlin tremor, Fitts' law timing, a speed cap, occasional overshoot; `grid(dpr, frame_ms)` puts every point on the physical pixel grid at one move per display frame |
| Clicks | Off-center aim, the box checked again before pressing, a human hold time, the hand drifting away after; elements brought into view with the wheel (option lists inside menus too) |
| Typing | Log-logistic flight times, digraphs, hand alternation, warm-up, fatigue, hesitations, pauses at letter/digit/punctuation boundaries and between number groups, typos corrected like a person, the real keys of the US, UK, PT or ES layout (Shift, AltGr, dead keys), chosen with `layout(..)` or else from the browser's language (`es-ES` → Spanish) |
| Fields | `fix_field` corrects a value in place (click at the mistake, delete only what is wrong), `append_to_field`, `type_verified` |
| Pauses | `pause`, `read`, `think`, `maybe_distraction`, with the mouse drifting meanwhile; `read_text` reads a text with the mouse resting near the next target |
| Scrolling | Gliding wheel pushes in gestures, in the person's style; `explore` looks over a page |
| Touch | `mobile(true)`: taps and swipes instead of clicks and the wheel |
| Lifetime | One person per browser context (`context.human()`, `page.human()`), dropped when the context or the browser closes, even when the window is closed by hand; each page has its own mouse position, forgotten when the page closes. `Human::new()` / `with_seed(n)` make a person you own yourself |

## Assertions (`expect`)

`oxidewright::expect` ports Playwright Test's web-first assertions (without the test runner).
They retry until they pass or the timeout expires (5 s by default, see
`expect::set_default_timeout`), running the injected script's own `expect()` like Playwright does:

```rust,ignore
use oxidewright::expect::expect;
use oxidewright::regex;

expect(&page.get_by_role("button")).to_be_visible().await?;
expect(&page.locator("#status")).not().to_have_text("Loading").timeout(1_000).await?;
expect(&page.locator("li")).to_have_text(["one", "two"]).await?;   // one entry per element
expect(&page.locator("h1")).to_contain_text(regex("welcome", "i")).await?;
expect(&page).to_have_url("/done").await?;                          // resolved against base_url
expect(&page).message("should be on the home page").to_have_title("Home").await?;
expect(&page.request().get(url).await?).to_be_ok().await?;
```

* Locators: `to_be_attached`, `to_be_checked` (`.checked(..)`, `.indeterminate(..)`),
  `to_be_disabled`, `to_be_editable`, `to_be_empty`, `to_be_enabled`, `to_be_focused`,
  `to_be_hidden`, `to_be_in_viewport` (`.ratio(..)`), `to_be_visible`, `to_contain_text`,
  `to_have_text` (`.use_inner_text(..)`, `.ignore_case(..)`; a string, a regex or a list),
  `to_have_accessible_name` / `_description` / `_error_message`, `to_have_attribute(name, value)`,
  `to_have_attribute_present(name)`, `to_have_class`, `to_contain_class`, `to_have_count`,
  `to_have_css` (`.pseudo(..)`), `to_have_id`, `to_have_js_property`, `to_have_role`,
  `to_have_value`, `to_have_values`, `to_match_aria_snapshot`, `to_have_screenshot`.
* Pages: `to_have_title`, `to_have_url` (string or regex, `.ignore_case(..)`),
  `to_have_url_matching(predicate)`, `to_match_aria_snapshot`, `to_have_screenshot`.
* API responses (`page.request()` / `route.fetch()` responses): `to_be_ok`.
* Failures are `Error::Expect(MatcherResult)` (name, pass, expected, actual, log, timeout, ARIA
  snapshot) and the message mirrors Playwright's, diffs included:

  ```text
  expect(locator).toHaveText(expected) failed

  Locator:  locator('#node')
  Expected: "hey"
  Received: "Text content"
  Timeout:  1000ms

  Call log:
    - Expect "toHaveText" locator('#node') with timeout 1000ms
    - waiting for locator('#node')
    -   locator resolved to <div id="node">Text content</div>
    -   unexpected value "Text content"
  ```

Differences with Playwright Test: no soft assertions or `expect.extend` (`expect.poll` and
`toPass` are `expect::poll` and `expect::to_pass`), no `AbortSignal` (drop the future instead) or `URLPattern`. ARIA snapshot templates are parsed with
`yaml-rust2`, so YAML syntax errors are worded differently. `to_have_screenshot` compares
with a PNG at an explicit path using Playwright's `pixelmatch` comparator (`threshold`,
`max_diff_pixels`, `max_diff_pixel_ratio`); a missing file is written and the assertion fails,
and `-actual.png` / `-diff.png` files are written next to it on mismatch. The `ssim-cie94`
comparator, snapshot path templates and `--update-snapshots` modes are not ported.

## Updating the bundled scripts

```sh
cargo run -p oxidewright-xtask -- update-injected patchright-core@<version>   # or a .tgz / directory
```

The bundled scripts are Playwright 1.65.1 with the Patchright 1.64.3 patches merged in, because
Patchright has no 1.65 release yet. When Playwright is ahead of Patchright, merge the
Patchright patches onto the newer Playwright (fails on a conflict):

```sh
cargo run -p oxidewright-xtask -- merge-patchright playwright-core@1.64.1 patchright-core@1.64.3 playwright-core@1.65.1
```

Both are plain Rust (the merge calls `git merge-file`); no Node.js is needed.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option. The bundled JavaScript (`src/js/generated`) and the device descriptors
(`src/devices.json`) are Apache-2.0, see [NOTICE](NOTICE).
