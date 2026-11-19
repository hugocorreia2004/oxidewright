//! The human-like input (`oxidewright::human`, not part of Playwright): the
//! events reach the page as a person's would, and the result is right.

use serde::Deserialize;

use crate::common::*;
use oxidewright::Page;
use oxidewright::human::{Human, KeyboardLayout, ReadStyle, ScrollDirection, Speed};

/// Records the trusted mouse, key, wheel and touch events of the page.
const RECORDER: &str = r#"<script>
  window.ev = [];
  for (const type of ['mousemove', 'mousedown', 'mouseup', 'click', 'keydown', 'keyup', 'wheel', 'touchstart', 'touchend']) {
    addEventListener(type, e => window.ev.push({
      type, t: performance.now(), x: e.clientX ?? (e.touches && e.touches[0] ? e.touches[0].clientX : null),
      y: e.clientY ?? (e.touches && e.touches[0] ? e.touches[0].clientY : null), key: e.key ?? null, code: e.code ?? null,
      shift: !!e.shiftKey, ctrl: !!e.ctrlKey, alt: !!e.altKey, trusted: e.isTrusted,
      target: e.target && e.target.id || null,
    }), true);
  }
</script>"#;

#[derive(Debug, Deserialize)]
struct Ev {
    #[serde(rename = "type")]
    kind: String,
    t: f64,
    x: Option<f64>,
    y: Option<f64>,
    key: Option<String>,
    code: Option<String>,
    shift: bool,
    ctrl: bool,
    alt: bool,
    trusted: bool,
    target: Option<String>,
}

async fn events(page: &Page) -> Vec<Ev> {
    page.evaluate("() => window.ev").await.unwrap()
}

async fn value(page: &Page, selector: &str) -> String {
    page.evaluate("s => document.querySelector(s).value")
        .arg(selector)
        .await
        .unwrap()
}

#[test]
fn click_moves_along_a_path_and_clicks_inside_the_element() {
    run(|t| async move {
        t.page
            .set_content(format!(
                r#"{RECORDER}<button id="b" style="position:absolute;left:600px;top:400px;width:120px;height:40px">OK</button>"#
            ))
            .await
            .unwrap();
        let human = Human::with_seed(1);
        human.click(&t.page.locator("#b")).await.unwrap();
        let ev = events(&t.page).await;
        assert!(ev.iter().all(|e| e.trusted));
        let moves = ev.iter().filter(|e| e.kind == "mousemove").count();
        assert!(moves >= 10, "a path, not a jump: {moves} moves");
        let click = ev.iter().find(|e| e.kind == "click").expect("a click");
        assert_eq!(click.target.as_deref(), Some("b"));
        let (x, y) = (click.x.unwrap(), click.y.unwrap());
        assert!((600.0..=720.0).contains(&x) && (400.0..=440.0).contains(&y));
        // Held like a finger, not 0 ms.
        let down = ev.iter().find(|e| e.kind == "mousedown").unwrap().t;
        let up = ev.iter().find(|e| e.kind == "mouseup").unwrap().t;
        assert!(up - down >= 30.0, "held {} ms", up - down);
    });
}

#[test]
fn types_with_the_keys_of_the_layout() {
    run(|t| async move {
        t.page
            .set_content(format!(r#"{RECORDER}<input id="i">"#))
            .await
            .unwrap();
        let human = Human::with_seed(2).layout(KeyboardLayout::Pt);
        let text = "Ana Conceição @x.pt 12";
        human
            .type_text(&t.page.locator("#i"), text)
            .errors(false)
            .await
            .unwrap();
        assert_eq!(value(&t.page, "#i").await, text);
        let ev = events(&t.page).await;
        let down = |key: &str| {
            ev.iter()
                .find(|e| e.kind == "keydown" && e.key.as_deref() == Some(key))
                .unwrap_or_else(|| panic!("no keydown for {key}"))
        };
        // A capital with Shift; `@` with AltGr (Ctrl+Alt) on Digit2; `ç` on its own key.
        let a = down("A");
        assert!(a.shift && a.code.as_deref() == Some("KeyA"));
        let at = down("@");
        // AltGr: Ctrl+Alt on Windows, its own key (no Ctrl/Alt) elsewhere.
        assert_eq!(at.ctrl && at.alt, cfg!(windows));
        assert!(!at.shift && at.code.as_deref() == Some("Digit2"));
        assert!(
            ev.iter()
                .any(|e| e.kind == "keydown" && e.key.as_deref() == Some("AltGraph"))
        );
        assert_eq!(down("ç").code.as_deref(), Some("Semicolon"));
        // `ã` is the dead key ~ then `a`.
        assert!(ev.iter().any(|e| e.kind == "keydown"
            && e.key.as_deref() == Some("Dead")
            && e.code.as_deref() == Some("Backslash")));
        assert!(
            ev.iter()
                .any(|e| e.kind == "keydown" && e.key.as_deref() == Some("Shift"))
        );
        // Keys are held.
        let keydowns = ev.iter().filter(|e| e.kind == "keydown").count();
        let keyups = ev.iter().filter(|e| e.kind == "keyup").count();
        assert_eq!(keydowns, keyups);
    });
}

#[test]
fn typos_are_corrected() {
    run(|t| async move {
        t.page.set_content(r#"<input id="i">"#).await.unwrap();
        // Several people, each with its own typos: the text always ends right.
        for seed in 10..13 {
            let human = Human::with_seed(seed);
            t.page
                .evaluate::<()>("() => document.querySelector('#i').value = ''")
                .await
                .unwrap();
            let text = "the quick brown fox jumps";
            human.type_text(&t.page.locator("#i"), text).await.unwrap();
            assert_eq!(value(&t.page, "#i").await, text, "seed {seed}");
        }
    });
}

#[test]
fn fixes_a_field_in_place_and_appends_to_it() {
    run(|t| async move {
        t.page
            .set_content(r#"<input id="i" value="Lopez" style="font:16px Arial;width:300px"><input id="j" value="Rua A  " style="font:16px Arial;width:300px">"#)
            .await
            .unwrap();
        let human = Human::with_seed(3).layout(KeyboardLayout::Pt);
        assert!(
            human
                .fix_field(&t.page.locator("#i"), "López")
                .await
                .unwrap()
        );
        assert_eq!(value(&t.page, "#i").await, "López");
        let expected = human
            .append_to_field(&t.page.locator("#j"), "2º", ", ")
            .await
            .unwrap();
        assert_eq!(expected.as_deref(), Some("Rua A, 2º"));
        assert_eq!(value(&t.page, "#j").await, "Rua A, 2º");
    });
}

#[test]
fn type_verified_gets_a_password_right() {
    run(|t| async move {
        t.page
            .set_content(r#"<input id="p" type="password" value="old">"#)
            .await
            .unwrap();
        let human = Human::with_seed(4).layout(KeyboardLayout::Es);
        human
            .type_verified(&t.page.locator("#p"), "Señal-2024!")
            .await
            .unwrap();
        assert_eq!(value(&t.page, "#p").await, "Señal-2024!");
    });
}

#[test]
fn type_verified_takes_the_typing_options() {
    run(|t| async move {
        t.page
            .set_content(format!(r#"{RECORDER}<input id="e" value="old@x.pt">"#))
            .await
            .unwrap();
        let human = Human::with_seed(12).layout(KeyboardLayout::Us);
        human
            .type_verified(&t.page.locator("#e"), "maria.s@example.com")
            .speed(Speed::Fast)
            .await
            .unwrap();
        assert_eq!(value(&t.page, "#e").await, "maria.s@example.com");
        // No typo was made (no Backspace).
        let ev = events(&t.page).await;
        assert!(
            !ev.iter()
                .any(|e| e.kind == "keydown" && e.key.as_deref() == Some("Backspace"))
        );
    });
}

#[test]
fn shift_is_held_across_capitals_and_released_before_the_next_key() {
    run(|t| async move {
        t.page
            .set_content(format!(r#"{RECORDER}<input id="i">"#))
            .await
            .unwrap();
        let human = Human::with_seed(13).layout(KeyboardLayout::Us);
        human
            .type_text(&t.page.locator("#i"), "ABc@d")
            .errors(false)
            .await
            .unwrap();
        assert_eq!(value(&t.page, "#i").await, "ABc@d");
        let ev = events(&t.page).await;
        let keys: Vec<&Ev> = ev
            .iter()
            .filter(|e| e.kind == "keydown" || e.kind == "keyup")
            .collect();
        let seq: Vec<String> = keys
            .iter()
            .map(|e| {
                format!(
                    "{}{}",
                    if e.kind == "keydown" { "+" } else { "-" },
                    e.key.as_deref().unwrap_or("")
                )
            })
            .collect();
        // One Shift for both capitals, released before `c`; another for `@`.
        assert_eq!(
            seq,
            [
                "+Shift", "+A", "-A", "+B", "-B", "-Shift", "+c", "-c", "+Shift", "+@", "-@",
                "-Shift", "+d", "-d"
            ],
            "{seq:?}"
        );
        // The key after a released Shift waits at least 20 ms.
        let shift_up = keys
            .iter()
            .find(|e| e.kind == "keyup" && e.key.as_deref() == Some("Shift"))
            .unwrap();
        let c_down = keys
            .iter()
            .find(|e| e.kind == "keydown" && e.key.as_deref() == Some("c"))
            .unwrap();
        assert!(
            c_down.t - shift_up.t >= 19.0,
            "{} ms",
            c_down.t - shift_up.t
        );
        // `@` is Shift+2 on a US keyboard.
        let at = keys
            .iter()
            .find(|e| e.kind == "keydown" && e.key.as_deref() == Some("@"))
            .unwrap();
        assert!(at.shift && at.code.as_deref() == Some("Digit2"));
    });
}

#[test]
fn idle_drifts_the_mouse_on_the_last_page() {
    run(|t| async move {
        t.page.set_content(RECORDER).await.unwrap();
        let human = Human::with_seed(14);
        human.move_to(&t.page, 400.0, 300.0).await.unwrap();
        let before = events(&t.page).await.len();
        let start = std::time::Instant::now();
        human.idle(1.5).await;
        let took = start.elapsed().as_secs_f64();
        assert!((1.4..3.5).contains(&took), "{took} s");
        let moves = events(&t.page).await[before..]
            .iter()
            .filter(|e| e.kind == "mousemove")
            .count();
        assert!(moves > 0, "the mouse drifted");
    });
}

#[test]
fn scrolls_with_the_wheel_to_click_a_far_element() {
    run(|t| async move {
        t.page
            .set_content(format!(
                r#"{RECORDER}<div style="height:3000px"></div><button id="b">far</button><div style="height:600px"></div>"#
            ))
            .await
            .unwrap();
        let human = Human::with_seed(5);
        human.click(&t.page.locator("#b")).await.unwrap();
        let ev = events(&t.page).await;
        let wheels = ev.iter().filter(|e| e.kind == "wheel").count();
        assert!(wheels > 5, "glides in small wheel events: {wheels}");
        assert!(
            ev.iter()
                .any(|e| e.kind == "click" && e.target.as_deref() == Some("b"))
        );
        let y: f64 = t.page.evaluate("() => scrollY").await.unwrap();
        assert!(y > 2000.0);
        // And back up with the wheel.
        human
            .scroll(&t.page, 1500.0, ScrollDirection::Up)
            .await
            .unwrap();
        let y2: f64 = t.page.evaluate("() => scrollY").await.unwrap();
        assert!(y2 < y - 1000.0, "{y} -> {y2}");
    });
}

#[test]
fn on_the_grid_moves_are_whole_pixels_one_per_frame() {
    run(|t| async move {
        t.page.set_content(RECORDER).await.unwrap();
        let human = Human::with_seed(6).grid(1.0, 1000.0 / 60.0);
        human.move_to(&t.page, 700.0, 500.0).await.unwrap();
        human.move_to(&t.page, 200.0, 150.0).await.unwrap();
        let ev = events(&t.page).await;
        let moves: Vec<&Ev> = ev.iter().filter(|e| e.kind == "mousemove").collect();
        assert!(moves.len() > 20);
        for m in &moves {
            assert_eq!(m.x.unwrap().fract(), 0.0);
            assert_eq!(m.y.unwrap().fract(), 0.0);
        }
        // Intervals between moves: a whole number of frames (~16.7 ms),
        // except the pause between the two movements.
        let mut gaps: Vec<f64> = moves.windows(2).map(|w| w[1].t - w[0].t).collect();
        gaps.sort_by(f64::total_cmp);
        let median = gaps[gaps.len() / 2];
        assert!((12.0..=22.0).contains(&median), "median gap {median} ms");
    });
}

#[test]
fn reads_a_text_with_the_mouse_resting_near_the_target() {
    run(|t| async move {
        t.page
            .set_content(format!(
                r#"{RECORDER}<p style="width:400px"><span id="txt">I accept the terms of use and the privacy policy of this site. <a href="/terms">Terms</a></span></p><input type="checkbox" id="c">"#
            ))
            .await
            .unwrap();
        let human = Human::with_seed(7);
        let secs = human
            .read_text(&t.page.locator("#txt"))
            .target(&t.page.locator("#c"))
            .style(ReadStyle::Rest)
            .await
            .unwrap();
        assert!((1.4..20.0).contains(&secs), "{secs} s");
        let moves = events(&t.page)
            .await
            .into_iter()
            .filter(|e| e.kind == "mousemove")
            .count();
        assert!(moves > 0);
    });
}

#[test]
fn taps_on_a_touch_page() {
    run(|t| async move {
        let context = t.browser.new_context().has_touch(true).await.unwrap();
        let page = context.new_page().await.unwrap();
        page.set_content(format!(
            r#"{RECORDER}<button id="b" style="position:absolute;left:100px;top:100px;width:120px;height:60px">OK</button>"#
        ))
        .await
        .unwrap();
        let human = Human::with_seed(8).mobile(true);
        human.click(&page.locator("#b")).await.unwrap();
        let ev = events(&page).await;
        assert!(ev.iter().any(|e| e.kind == "touchstart"));
        assert!(
            ev.iter()
                .any(|e| e.kind == "click" && e.target.as_deref() == Some("b"))
        );
        assert!(!ev.iter().any(|e| e.kind == "mousedown" && !e.trusted));
        context.close().await.unwrap();
    });
}

#[test]
fn press_sends_a_held_key() {
    run(|t| async move {
        t.page
            .set_content(format!(r#"{RECORDER}<input id="i" autofocus>"#))
            .await
            .unwrap();
        t.page.locator("#i").focus().await.unwrap();
        let human = Human::with_seed(9);
        human.press(&t.page, "Enter").await.unwrap();
        let ev = events(&t.page).await;
        let down = ev
            .iter()
            .find(|e| e.kind == "keydown" && e.key.as_deref() == Some("Enter"))
            .unwrap();
        let up = ev
            .iter()
            .find(|e| e.kind == "keyup" && e.key.as_deref() == Some("Enter"))
            .unwrap();
        assert!(up.t - down.t >= 25.0);
    });
}

#[test]
fn each_page_keeps_its_own_mouse_and_a_closed_page_is_forgotten() {
    run(|t| async move {
        let human = Human::with_seed(11);
        let other = t.context.new_page().await.unwrap();
        human.move_to(&t.page, 300.0, 200.0).await.unwrap();
        human.move_to(&other, 700.0, 500.0).await.unwrap();
        // Moving in the other page does not move the mouse of the first one.
        assert_eq!(human.mouse_position(&t.page), (300.0, 200.0));
        assert_eq!(human.mouse_position(&other), (700.0, 500.0));
        assert_eq!(human.remembered_pages(), 2);
        other.close().await.unwrap();
        for _ in 0..50 {
            if human.remembered_pages() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(human.remembered_pages(), 1);
        // The person and the first page go on as before.
        assert_eq!(human.mouse_position(&t.page), (300.0, 200.0));
        human.move_to(&t.page, 320.0, 260.0).await.unwrap();
    });
}

#[test]
fn the_context_creates_its_person_and_drops_it_when_it_closes() {
    run(|t| async move {
        let context = t.browser.new_context().await.unwrap();
        let page = context.new_page().await.unwrap();
        // The same person every time, from the context or from its pages.
        let human = page.human();
        assert_eq!(human.traits(), context.human().traits());
        human.move_to(&page, 300.0, 200.0).await.unwrap();
        assert_eq!(page.human().mouse_position(&page), (300.0, 200.0));
        assert_eq!(human.remembered_pages(), 1);
        context.close().await.unwrap();
        // Closing the context forgot everything and let the person go.
        assert_eq!(human.remembered_pages(), 0);
        // A configured person.
        let other = t.browser.new_context().await.unwrap();
        other.set_human(Human::with_seed(42).layout(KeyboardLayout::Pt));
        assert_eq!(other.human().traits(), Human::with_seed(42).traits());
        other.close().await.unwrap();
    });
}

#[test]
fn closing_the_browser_drops_the_person() {
    run(|_t| async move {
        let mut launch = oxidewright::chromium().launch();
        if let Ok(path) = std::env::var("OXIDEWRIGHT_CHROME") {
            launch = launch.executable_path(path);
        }
        let browser = launch.await.unwrap();
        let page = browser.new_page().await.unwrap();
        let human = page.human();
        human.move_to(&page, 300.0, 200.0).await.unwrap();
        assert_eq!(human.remembered_pages(), 1);
        browser.close().await.unwrap();
        assert_eq!(human.remembered_pages(), 0);
    });
}

#[test]
fn picks_the_keyboard_of_the_browser_language() {
    run(|t| async move {
        // (browser language, text, key, expected code, expected Shift, AltGr)
        for (locale, text, key, code, shift, altgr) in [
            ("es-ES", "año@x", "ñ", "Semicolon", false, false),
            ("es-ES", "año@x", "@", "Digit2", false, true),
            ("en-GB", "a@b", "@", "Quote", true, false),
            ("en-US", "a@b", "@", "Digit2", true, false),
        ] {
            let context = t.browser.new_context().locale(locale).await.unwrap();
            let page = context.new_page().await.unwrap();
            page.set_content(format!(r#"{RECORDER}<input id="i">"#))
                .await
                .unwrap();
            let human = page.human();
            human
                .type_text(&page.locator("#i"), text)
                .errors(false)
                .await
                .unwrap();
            assert_eq!(value(&page, "#i").await, text, "{locale}");
            let ev = events(&page).await;
            let down = ev
                .iter()
                .find(|e| e.kind == "keydown" && e.key.as_deref() == Some(key))
                .unwrap_or_else(|| panic!("{locale}: no keydown for {key}"));
            assert_eq!(down.code.as_deref(), Some(code), "{locale} {key}");
            assert_eq!(down.shift, shift, "{locale} {key} shift");
            assert_eq!(
                down.ctrl && down.alt,
                altgr && cfg!(windows),
                "{locale} {key} altgr"
            );
            context.close().await.unwrap();
        }
        // A layout chosen by the script wins over the browser language.
        let context = t.browser.new_context().locale("en-GB").await.unwrap();
        context.set_human(Human::new().layout(KeyboardLayout::Pt));
        let page = context.new_page().await.unwrap();
        assert_eq!(
            page.human().keyboard_layout(&page).await,
            KeyboardLayout::Pt
        );
        context.close().await.unwrap();
    });
}
