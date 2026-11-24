//! `context.new_page()` opens a TAB of the context's window, like Playwright
//! (`Target.createTarget` without `newWindow`), and a page that is no longer
//! the front tab keeps working: it is visible, animates, takes screenshots
//! and scrolls with the wheel (focus emulation keeps every page visible).
//! A new context still gets its own window: that is Chrome, not us.
//!
//! A separate test binary so it can run with a visible window, where tabs
//! and windows really differ: `OXIDEWRIGHT_HEADED=1 cargo test -p
//! oxidewright --test tabs`. Without it the browser is headless.

use std::time::Duration;

use oxidewright::{Browser, BrowserContext, Page, chromium};
use serde_json::{Value, json};

/// Every step must answer well within this (a stalled background tab never
/// answers at all).
const PRAZO: Duration = Duration::from_secs(10);

async fn launch() -> Browser {
    let mut launch = chromium().launch();
    if let Ok(path) = std::env::var("OXIDEWRIGHT_CHROME") {
        launch = launch.executable_path(path);
    }
    if std::env::var("OXIDEWRIGHT_HEADED").is_ok_and(|v| v == "1") {
        launch = launch.headless(false);
    }
    launch.await.expect("failed to launch the browser")
}

async fn within<T>(what: &str, f: impl std::future::IntoFuture<Output = T>) -> T {
    tokio::time::timeout(PRAZO, f.into_future())
        .await
        .unwrap_or_else(|_| panic!("{what} did not answer within {PRAZO:?}"))
}

/// The id of the window that holds the page.
async fn window_of(context: &BrowserContext, page: &Page) -> i64 {
    let session = context.new_cdp_session(page).await.unwrap();
    let r: Value = session
        .send("Browser.getWindowForTarget", json!({}))
        .await
        .unwrap();
    r["windowId"].as_i64().expect("windowId")
}

const LONG: &str = "<body style='margin:0'><div style='height:5000px'>long</div></body>";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_pages_are_tabs_of_the_same_window_and_the_background_one_works() {
    let browser = launch().await;
    let context = browser.new_context().await.unwrap();
    let back = context.new_page().await.unwrap();
    back.set_content(LONG).await.unwrap();
    // Opened after it: `back` is no longer the front tab.
    let front = context.new_page().await.unwrap();
    front.set_content("<p>front</p>").await.unwrap();

    assert_eq!(
        window_of(&context, &back).await,
        window_of(&context, &front).await,
        "both pages should be tabs of one window"
    );

    let state: String = within(
        "evaluate",
        back.evaluate("() => document.visibilityState + '/' + document.hasFocus()"),
    )
    .await
    .unwrap();
    assert_eq!(state, "visible/true");

    // It still produces frames.
    let raf: bool = within(
        "requestAnimationFrame",
        back.evaluate("() => new Promise(r => requestAnimationFrame(() => r(true)))"),
    )
    .await
    .unwrap();
    assert!(raf);

    let png = within("screenshot", back.screenshot()).await.unwrap();
    assert!(png.len() > 100, "screenshot of {} bytes", png.len());

    within("mouse.move", back.mouse().r#move(50, 50))
        .await
        .unwrap();
    within("mouse.wheel", back.mouse().wheel(0, 300))
        .await
        .unwrap();
    within(
        "scrollY",
        back.wait_for_function("() => window.scrollY >= 300")
            .into_future(),
    )
    .await
    .unwrap();
    let scroll: f64 = back.evaluate("() => window.scrollY").await.unwrap();
    assert!(scroll >= 300.0, "scrollY = {scroll}");

    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_context_gets_its_own_window() {
    let browser = launch().await;
    let a = browser.new_context().await.unwrap();
    let b = browser.new_context().await.unwrap();
    let pa = a.new_page().await.unwrap();
    let pb = b.new_page().await.unwrap();
    assert_ne!(window_of(&a, &pa).await, window_of(&b, &pb).await);
    browser.close().await.unwrap();
}
