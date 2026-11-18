<h1 align="center">oxidewright</h1>

<p align="center">
  <b>Playwright for Rust: no Node.js, Patchright-level stealth, human-like input.</b>
</p>

<p align="center">
  <a href="https://github.com/hugocorreia2004/oxidewright/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/hugocorreia2004/oxidewright/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="MSRV 1.85" src="https://img.shields.io/badge/rust-1.85%2B-orange">
  <img alt="License MIT or Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue">
</p>

oxidewright lets a Rust program drive Chrome the way [Playwright](https://playwright.dev)
does: open pages, find elements, click, type, wait for the network, mock requests, take
screenshots, save logins. It has Playwright's API (version 1.65, Chromium only) and is written
in Rust from the ground up, so there is **no Node.js** to install or run next to your program.

It is built for automation that must not look automated: it never sends the protocol commands
that pages detect (like [Patchright](https://github.com/Kaliiiiiiiiii-Vinyzu/patchright)), and
it can move the mouse and type like a person.

**Contents:** [Install](#install) · [Quick start](#quick-start) · [Guide](#guide) ·
[Human-like input](#human-like-input) · [Stealth](#stealth) · [Status](#status) ·
[Coming from Playwright](#coming-from-playwright) · [Credits and license](#credits-and-license)

## Install

```toml
[dependencies]
oxidewright = { git = "https://github.com/hugocorreia2004/oxidewright" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

You need Rust 1.85 or newer and Chrome or Chromium installed (it is found on its own; or pass
`.executable_path("...")` to `launch()`). oxidewright is not on crates.io yet.

## Quick start

```rust
use oxidewright::chromium;

#[tokio::main]
async fn main() -> oxidewright::Result<()> {
    let browser = chromium().launch().await?;
    let page = browser.new_page().await?;
    page.goto("https://example.com").await?;

    println!("title: {}", page.title().await?);
    page.get_by_role("link").first().click().await?;
    page.screenshot().path("page.png").await?;

    browser.close().await?;
    Ok(())
}
```

Every call that talks to the browser is `async` and returns `oxidewright::Result`. Options are
methods chained before `.await`: `page.goto(url).timeout(5_000).await`.

## Guide

### Open a browser

```rust
use oxidewright::{chromium, ProxySettings};

// A new, temporary browser. Visible window, through a proxy.
let browser = chromium()
    .launch()
    .headless(false)
    .proxy(ProxySettings::new("http://proxy.example:8080"))
    .await?;

// A browser with a profile folder that is kept (logins survive restarts).
let context = chromium().launch_persistent_context("profiles/ana").await?;
let page = context.pages()[0].clone(); // the window it opened with

// A browser that is already running (an anti-detect browser profile, say),
// started with --remote-debugging-port=9222.
let browser = chromium().connect_over_cdp("http://127.0.0.1:9222").await?;
let context = browser.contexts()[0].clone(); // its tabs: context.pages()
```

Several browsers run at the same time from one program: give each its own tokio task. They do
not wait for each other, since each one has its own connection.

### Contexts and pages

A **context** is an independent browser session (its own cookies, storage, cache), like an
incognito window. A **page** is a tab.

```rust
use oxidewright::{devices, ViewportSize};

let context = browser
    .new_context()
    .locale("es-ES")
    .timezone_id("Europe/Madrid")
    .viewport(Some(ViewportSize { width: 1366, height: 768 }))
    .await?;
let page = context.new_page().await?;

// Or a phone: viewport, user agent, touch, device scale factor.
let phone = browser.new_context().device(&devices()["iPhone 15"]).await?;
```

### Navigate and wait

```rust
use oxidewright::LoadState;

page.goto("https://example.com/login").await?;
page.goto("https://example.com/feed").wait_until(LoadState::NetworkIdle).await?;
page.wait_for_url("**/dashboard").await?;     // after a redirect
page.wait_for_load_state().state(LoadState::DomContentLoaded).await?;
page.reload().await?;
page.go_back().await?;
```

### Find elements

A **locator** describes how to find an element. It is resolved when it is used, so it always
finds the current element, and it waits for it to appear.

```rust
use oxidewright::{GetByRoleOptions, LocatorOptions};

page.get_by_role_with("button", GetByRoleOptions::new().name("Sign in")); // a button by its text
page.get_by_label("Email");                 // an input by its <label>
page.get_by_placeholder("you@example.com");
page.get_by_text("Welcome back");
page.get_by_test_id("submit");              // data-testid="submit"
page.locator("form#login input[name=user]"); // any CSS selector

// Narrow it down.
page.get_by_role("listitem").filter(LocatorOptions::new().has_text("Banana"));
page.locator(".result").nth(2);
page.frame_locator("iframe#payment").get_by_label("Card number"); // inside an iframe
```

If a locator matches several elements where one is expected, the action fails with a clear
error instead of picking one at random (Playwright's *strict mode*).

### Act on elements

Before acting, oxidewright waits until the element is visible, enabled, stable (not moving)
and not covered by something else, like Playwright. No sleeps needed.

```rust
page.get_by_label("Email").fill("ana@example.com").await?;
page.get_by_label("Password").fill("secret").await?;
page.get_by_label("Password").press("Enter").await?;
page.get_by_label("I accept the terms").check().await?;
page.get_by_label("Country").select_option("ES").await?;
page.get_by_label("Photo").set_input_files("photo.jpg").await?;
page.get_by_text("Menu").hover().await?;
page.get_by_role("button").click().timeout(5_000).await?; // fail after 5 s instead of 30 s
```

### Read the page

```rust
let text = page.locator("h1").inner_text().await?;
let value = page.get_by_label("Email").input_value().await?;
let count = page.locator(".result").count().await?;
let visible = page.get_by_text("Error").is_visible().await?;

// Run JavaScript; results come back as any serde type.
let title: String = page.evaluate("() => document.title").await?;
let sum: i32 = page.evaluate("([a, b]) => a + b").arg((1, 2)).await?;
```

### Check the result (assertions)

`expect` retries until the condition holds or the timeout (5 s) passes, so it suits pages
that change.

```rust
use oxidewright::expect::expect;

expect(&page.locator("h1")).to_have_text("Welcome, Ana").await?;
expect(&page.get_by_role("alert")).to_be_hidden().await?;
expect(&page).to_have_url("https://example.com/dashboard").await?;
```

### Network: watch, mock, block

```rust
use oxidewright::Route;

// Log every request.
page.on_request(|request| println!("{} {}", request.method(), request.url()));

// Answer an API call with your own data.
page.route("**/api/user", |route: Route| async move {
    route.fulfill().json(&serde_json::json!({ "name": "Ana" })).await
}).await?;

// Block images.
page.route("**/*.{png,jpg}", |route: Route| async move { route.abort().await }).await?;

// Wait for a response triggered by an action.
let response = page
    .expect_response("**/api/orders", page.get_by_text("Load more").click())
    .await?;
println!("{}", response.text().await?);
```

### Popups, dialogs and downloads

```rust
let popup = page.expect_popup(page.get_by_text("Open in new tab").click()).await?;
popup.wait_for_load_state().await?;

page.on_dialog(|dialog| {
    tokio::spawn(async move { dialog.accept(None).await }); // OK on alert/confirm
});

let download = page.expect_download(page.get_by_text("Export").click()).await?;
download.save_as("export.csv").await?;
```

### Keep a login (cookies and storage)

```rust
// After signing in once, save cookies and localStorage...
context.storage_state().path("state.json").await?;
// ...and start later sessions already signed in.
let context = browser.new_context().storage_state("state.json").await?;
```

### Screenshots, PDF, traces

```rust
page.screenshot().path("page.png").full_page(true).await?;
page.locator("#chart").screenshot().path("chart.png").await?;
page.pdf().path("page.pdf").await?;

// A trace of the session, viewable with `npx playwright show-trace trace.zip`.
context.tracing().start().screenshots(true).await?;
// ... do things ...
context.tracing().stop().path("trace.zip").await?;
```

### Errors and timeouts

Actions wait up to 30 s and navigations up to 30 s by default; change them per call
(`.timeout(ms)`) or per context (`context.set_default_timeout(ms)`). Errors say what failed
and why, like Playwright's:

```text
locator.click: Timeout 5000ms exceeded.
Call log:
  - waiting for getByRole('button', { name: 'Sign in' })
  - element is not enabled
```

`err.is_timeout()` and `err.is_target_closed()` tell the common cases apart.

## Human-like input

Playwright moves the mouse in a straight jump and types at a fixed speed, which is easy to
tell apart from a person. `oxidewright::human` (not part of Playwright) does it the way people
do.

```rust
use oxidewright::human::{PauseKind, ReadingKind};

let human = page.human(); // this browser's person: created on first use, the same one after

human.read(&page, ReadingKind::ScanningPage).await;      // look at the page, mouse drifting
human.click(&page.get_by_label("Name")).await?;          // move there and click
human.type_text(&page.get_by_label("Name"), "María José").await?;
human.type_text(&page.get_by_label("Birth date"), "14031987").groups([2, 2, 4]).await?;
human.type_verified(&page.get_by_label("Email"), "maria@example.com").await?;
human.click(&page.get_by_role("button")).await?;
human.pause(PauseKind::AfterSubmit).await;
```

**One person per browser.** `page.human()` gives the person of that browser: when it is
created, it draws its traits once and keeps them (how fast it is, how fast it types, how long
it holds keys, how much its hand trembles, how it scrolls, whether it reads before clicking),
so everything it does looks like the same person. It is dropped when the browser closes. To
choose it instead: `context.set_human(Human::with_seed(7))` (the same seed always gives the
same person).

| What | How it looks like a person |
|---|---|
| Mouse | Curved paths with hand tremor; slower and more careful on small or far targets; sometimes goes past the target and corrects. Each page keeps its own mouse position, so there are no jumps |
| Clicks | Aims off-center, holds the button for a human time, the hand drifts away after. Elements below the fold are reached with the mouse wheel, not jumped to |
| Typing | Each key is pressed and released with human timings: faster on common letter pairs, slower at the start, after a while, around `@`, between letters and digits and between groups of a number. Makes typos and corrects them |
| Keyboard | Uses the real keys of the US, UK, Portuguese or Spanish keyboard: `@` is AltGr+2 in Spain, `ñ` has its own key, `á` is the accent key then `a`. Taken from the browser's language, or set with `Human::new().layout(KeyboardLayout::Es)` |
| Fields | `fix_field` corrects a wrong value by clicking where the mistake is; `type_verified` checks what was typed and fixes it |
| Pauses and reading | `pause` after clicks, submits, page loads; `read` before acting; `read_text` reads a text with the mouse resting near what comes next |
| Scrolling | `scroll` with wheel pushes that glide and slow down, in the person's own style |
| Phones | `Human::new().mobile(true)`: taps and swipes instead of clicks and the wheel |

## Stealth

Websites can see some of the commands an automation tool sends to Chrome. Playwright and
Puppeteer send them; oxidewright does not:

- No `Runtime.enable`, `Log.enable` or `Performance.enable`, the best-known automation traces.
- Its scripts run in isolated worlds (they share the page's elements, not its JavaScript),
  created only when needed, with random names.
- `page.evaluate` reaches the page's own JavaScript through the page itself (Patchright's
  technique), so the page cannot tell how it was called.
- Screenshots never bring the window to the front; interception is only on while you use it.

Some features *are* visible to pages when you turn them on, such as console events, JavaScript
coverage, and every emulation option (a viewport, user agent or locale you set is what the
page sees). The [full list](oxidewright/README.md#stealth) says which.

Stealth here means not leaving automation traces. It does not change the browser's
fingerprint (screen, fonts, WebGL...); use a real Chrome profile or an anti-detect browser
through `connect_over_cdp` for that.

## Status

Implemented: browsers, contexts and pages (`launch`, persistent profiles, `connect_over_cdp`),
frames including cross-origin iframes, locators and every action with Playwright's waiting
rules, keyboard, mouse and touch, network interception, events, emulation and devices,
cookies and storage state, screenshots and PDF, HAR, the `request` API, tracing, video,
clock, coverage, workers and `expect` assertions.

Not yet: the inspector (`pause`), WebMCP, `launch_server`/`connect`, client certificates on
browser contexts, zipped HAR files, per-action events in traces. Only Chromium. The
[feature matrix](oxidewright/README.md#feature-matrix) has the details.

Over two thousand of Playwright's own tests, ported to Rust, run on every change on Linux and
Windows.

## Coming from Playwright

The same classes and methods, in `snake_case`; options become chained methods:

| Playwright (JS) | oxidewright |
|---|---|
| `await page.goto(url, { waitUntil: 'networkidle' })` | `page.goto(url).wait_until(LoadState::NetworkIdle).await?` |
| `await page.getByRole('button', { name: 'OK' }).click()` | `page.get_by_role_with("button", GetByRoleOptions::new().name("OK")).click().await?` |
| `await page.locator('#q').fill('rust')` | `page.locator("#q").fill("rust").await?` |
| `await page.evaluate(([a, b]) => a + b, [1, 2])` | `page.evaluate::<i32>("([a, b]) => a + b").arg((1, 2)).await?` |
| `page.on('request', r => ...)` | `page.on_request(\|r\| ...)` |
| `await expect(locator).toHaveText('Hi')` | `expect(&locator).to_have_text("Hi").await?` |

Every class, method by method: [docs/from-playwright.md](oxidewright/docs/from-playwright.md).
More in the [library README](oxidewright/README.md) and the runnable
[examples](oxidewright/examples) (`cargo run -p oxidewright --example locators_forms`).

## Repository layout

| Path | What |
|---|---|
| [`oxidewright/`](oxidewright) | the library, its [reference README](oxidewright/README.md), examples and the ported test suite |
| [`oxidewright/xtask/`](oxidewright/xtask) | maintenance: regenerates the bundled Playwright/Patchright scripts |
| `src/`, `chromiumoxide_*/` | the DevTools Protocol layer, a fork of [chromiumoxide](https://github.com/mattsse/chromiumoxide) ([docs/protocol-layer.md](docs/protocol-layer.md)) |

## Credits and license

The protocol layer started as [chromiumoxide](https://github.com/mattsse/chromiumoxide) by
Matthias Seitz and contributors; its history is kept here. The bundled injected scripts come
from [Playwright](https://github.com/microsoft/playwright) and
[Patchright](https://github.com/Kaliiiiiiiiii-Vinyzu/patchright) (Apache-2.0); see
[NOTICE](oxidewright/NOTICE).

oxidewright is an independent project, **not affiliated with, endorsed by or sponsored by
Microsoft** or the Playwright or Patchright projects. "Playwright" is a trademark of Microsoft
Corporation.

Licensed under either the [Apache License, Version 2.0](LICENSE-APACHE) or the
[MIT license](LICENSE-MIT), at your option.
