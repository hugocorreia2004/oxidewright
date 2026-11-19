//! Typing character by character like a person.

use std::collections::{HashMap, HashSet};
use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use super::keyboard::{
    KeyboardLayout, MOD_ALT, MOD_CTRL, MOD_SHIFT, Stroke, base_letter, key_event, plan,
};
use super::math::{Rnd, clamp, hand_multiplier};
use super::{Human, Traits, page_of};
use crate::{CDPSession, Locator, Page, Result};

/// How fast to type, relative to the person's own rhythm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Speed {
    /// Fast.
    Fast,
    /// The person's usual speed.
    #[default]
    Normal,
    /// Slow.
    Slow,
    /// Careful (slowest, most regular).
    Careful,
}

impl Speed {
    /// Log-logistic (alpha ms, beta) of the time between keys.
    fn params(self) -> (f64, f64) {
        match self {
            Speed::Fast => (60.0, 4.5),
            Speed::Normal => (95.0, 3.8),
            Speed::Slow => (140.0, 3.2),
            Speed::Careful => (200.0, 2.8),
        }
    }
}

/// QWERTY neighbours, for typos.
fn adjacent(c: char) -> &'static str {
    match c {
        'a' => "sqwz",
        'b' => "vghn",
        'c' => "xdfv",
        'd' => "erfcxs",
        'e' => "rdsw34",
        'f' => "rtgvcd",
        'g' => "tyhbvf",
        'h' => "yujnbg",
        'i' => "ujko89",
        'j' => "uikmnh",
        'k' => "iolmj",
        'l' => "opk",
        'm' => "njk",
        'n' => "bhjm",
        'o' => "iklp90",
        'p' => "ol0",
        'q' => "wa12",
        'r' => "edft45",
        's' => "wedxza",
        't' => "rfgy56",
        'u' => "yhjki78",
        'v' => "cfgb",
        'w' => "qase23",
        'x' => "zsdc",
        'y' => "tghu67",
        'z' => "xsa",
        '1' => "2q",
        '2' => "13qw",
        '3' => "24we",
        '4' => "35er",
        '5' => "46rt",
        '6' => "57ty",
        '7' => "68yu",
        '8' => "79ui",
        '9' => "80io",
        '0' => "9op",
        _ => "",
    }
}

/// Flight time multiplier of common letter pairs (default 1).
fn digraph(pair: &str) -> f64 {
    static TABLE: OnceLock<HashMap<&'static str, f64>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        [
            ("th", 0.76),
            ("he", 0.77),
            ("an", 0.78),
            ("en", 0.78),
            ("er", 0.77),
            ("te", 0.78),
            ("or", 0.78),
            ("on", 0.79),
            ("se", 0.78),
            ("de", 0.77),
            ("ar", 0.78),
            ("al", 0.79),
            ("ra", 0.78),
            ("do", 0.78),
            ("os", 0.77),
            ("es", 0.77),
            ("as", 0.77),
            ("ta", 0.78),
            ("re", 0.77),
            ("da", 0.78),
            ("co", 0.79),
            ("ou", 0.80),
            ("in", 0.78),
            ("ed", 0.90),
            ("st", 0.91),
            ("tr", 0.92),
            ("io", 0.91),
            ("we", 0.90),
            ("lo", 0.92),
            ("po", 0.91),
            ("ui", 0.90),
            ("ss", 0.78),
            ("rr", 0.79),
            ("ll", 0.77),
            ("ee", 0.78),
            ("oo", 0.79),
            ("tt", 0.80),
            ("nn", 0.79),
            ("pp", 0.80),
            ("ce", 1.15),
            ("nu", 1.18),
            ("my", 1.20),
            ("br", 1.12),
            ("xq", 1.30),
            ("zw", 1.35),
            ("qk", 1.32),
            ("xz", 1.35),
            ("wq", 1.30),
        ]
        .into_iter()
        .collect()
    });
    table.get(pair).copied().unwrap_or(1.0)
}

/// Indexes where a new block starts, to type numbers in parts (a pause
/// between blocks): a date `[2, 2, 4]` → {2, 4}; a phone `[3, 3, 3]` → {3, 6}.
pub(crate) fn group_boundaries(groups: &[usize], n: usize) -> HashSet<usize> {
    let mut out = HashSet::new();
    if groups.len() < 2 {
        return out;
    }
    let mut acc = 0;
    for g in &groups[..groups.len() - 1] {
        acc += g;
        if acc > 0 && acc < n {
            out.insert(acc);
        }
    }
    out
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// Extra pause range (ms) where a person slows down inside a token: switching
/// between letters and digits, around `@`, around structuring punctuation.
pub(crate) fn boundary_bump(prev: char, ch: char) -> Option<(f64, f64)> {
    const PUNCT: &str = ".,*/-_+#%ºª";
    if (is_digit(prev) && ch.is_alphabetic()) || (prev.is_alphabetic() && is_digit(ch)) {
        return Some((90.0, 240.0));
    }
    if ch == '@' || prev == '@' {
        return Some((120.0, 300.0));
    }
    if PUNCT.contains(ch) || PUNCT.contains(prev) {
        return Some((70.0, 200.0));
    }
    None
}

fn is_upper(c: char) -> bool {
    c.is_uppercase()
}

/// The minimum between releasing Shift and the next key, whatever the pace.
const AFTER_SHIFT_MIN_MS: f64 = 20.0;

/// The left-hand keys for the choice of Shift: letters, digits and the
/// backquote only. The shifted symbols (`!`, `@`, `#`, `$`, `%`, `~`) do not
/// count, so their Shift is usually the left one.
const SHIFT_LEFT_HAND: &str = "qwertasdfgzxcvb12345`";

/// The Shift key (code, location) pressed to type `ch`: the hand that does
/// not type the key presses it (right Shift for a left-hand key 80% of the
/// time, left Shift otherwise 90% of the time).
pub(crate) fn shift_key(rnd: &Rnd, ch: char) -> (&'static str, i64) {
    let left = SHIFT_LEFT_HAND.contains(base_letter(ch));
    let right = if left {
        rnd.random() < 0.8
    } else {
        rnd.random() < 0.1
    };
    if right {
        ("ShiftRight", 2)
    } else {
        ("ShiftLeft", 1)
    }
}

/// Presses the keys of one text: each character with the physical keys of
/// the layout (Shift, AltGr, dead key), Shift kept down between consecutive
/// capitals and released before a key without it.
struct Writer<'a> {
    human: &'a Human,
    page: &'a Page,
    session: CDPSession,
    layout: KeyboardLayout,
    shift: Option<(&'static str, i64)>,
}

impl Writer<'_> {
    async fn send(&self, params: serde_json::Value) -> Result<()> {
        self.human
            .guard(
                "pressing a key",
                self.session.send("Input.dispatchKeyEvent", params),
            )
            .await
            .map(|_| ())
    }

    async fn character(&mut self, ch: char, dwell_ms: f64) -> Result<()> {
        let strokes = plan(ch, self.layout).unwrap_or_else(|| {
            vec![Stroke {
                code: String::new(),
                key: ch.to_string(),
                text: ch.to_string(),
                shift: false,
                altgr: false,
            }]
        });
        let rnd = self.human.rnd();
        for (i, stroke) in strokes.iter().enumerate() {
            if i > 0 {
                // Dead key, then the letter.
                self.human.sleep(rnd.uniform(60.0, 160.0)).await;
            }
            let dwell = if i == strokes.len() - 1 {
                dwell_ms
            } else {
                clamp(dwell_ms * rnd.uniform(0.8, 1.1), 30.0, 180.0)
            };
            self.stroke(stroke, ch, dwell).await?;
        }
        Ok(())
    }

    /// A key without a character (Backspace...): releases Shift first.
    async fn special(&mut self, key: &str, dwell_ms: f64) -> Result<()> {
        self.release_shift().await?;
        self.human.key_press(self.page, key, dwell_ms).await
    }

    async fn stroke(&mut self, s: &Stroke, ch: char, dwell_ms: f64) -> Result<()> {
        let rnd = self.human.rnd();
        if s.shift {
            self.press_shift(ch).await?;
        } else {
            self.release_shift().await?;
        }
        let shift_bits = if self.shift.is_some() { MOD_SHIFT } else { 0 };
        // AltGr is Ctrl + Alt on Windows (a real keyboard sends left Ctrl and
        // right Alt). Elsewhere it is its own key, AltGraph, without Ctrl or
        // Alt: Chrome on Linux takes a Ctrl+Alt key as a shortcut and types
        // nothing. What counts is the system Chrome runs on, which is the
        // one this program runs on when it launches it.
        let windows_altgr = s.altgr && cfg!(windows);
        if s.altgr {
            if windows_altgr {
                self.send(key_event(
                    true,
                    "Control",
                    "ControlLeft",
                    "",
                    shift_bits | MOD_CTRL,
                    1,
                ))
                .await?;
                self.human.sleep(rnd.uniform(1.0, 6.0)).await;
            }
            let held = if windows_altgr { MOD_CTRL | MOD_ALT } else { 0 };
            self.send(key_event(
                true,
                "AltGraph",
                "AltRight",
                "",
                shift_bits | held,
                2,
            ))
            .await?;
            self.human.sleep(rnd.uniform(35.0, 100.0)).await;
        }
        let mods = shift_bits | if windows_altgr { MOD_CTRL | MOD_ALT } else { 0 };
        self.send(key_event(true, &s.key, &s.code, &s.text, mods, 0))
            .await?;
        self.human.sleep(dwell_ms).await;
        self.send(key_event(false, &s.key, &s.code, "", mods, 0))
            .await?;
        if s.altgr {
            self.human.sleep(rnd.uniform(15.0, 60.0)).await;
            let held = if windows_altgr { MOD_CTRL } else { 0 };
            self.send(key_event(
                false,
                "AltGraph",
                "AltRight",
                "",
                shift_bits | held,
                2,
            ))
            .await?;
            if windows_altgr {
                self.send(key_event(
                    false,
                    "Control",
                    "ControlLeft",
                    "",
                    shift_bits,
                    1,
                ))
                .await?;
            }
        }
        Ok(())
    }

    async fn press_shift(&mut self, ch: char) -> Result<()> {
        if self.shift.is_some() {
            return Ok(());
        }
        let rnd = self.human.rnd();
        let shift = shift_key(rnd, ch);
        self.shift = Some(shift);
        self.send(key_event(true, "Shift", shift.0, "", MOD_SHIFT, shift.1))
            .await?;
        self.human.sleep(rnd.uniform(35.0, 110.0)).await;
        Ok(())
    }

    async fn release_shift(&mut self) -> Result<()> {
        let Some(shift) = self.shift else {
            return Ok(());
        };
        let rnd = self.human.rnd();
        self.human.sleep(rnd.uniform(5.0, 40.0)).await;
        self.send(key_event(false, "Shift", shift.0, "", 0, shift.1))
            .await?;
        self.shift = None;
        // The next key does not follow at once (a person takes tens of ms),
        // and the pace never shrinks this below the minimum.
        let wait = (rnd.uniform(25.0, 110.0) * self.human.pace()).max(AFTER_SHIFT_MIN_MS);
        self.human.sleep_plain(wait).await;
        Ok(())
    }
}

/// A field focused by our own click this recently was focused by that click.
const OUR_CLICK_FOCUS: Duration = Duration::from_secs(12);

impl Human {
    /// Press a key without a character (Tab, Enter, arrows...) with this
    /// person's hold time.
    pub async fn press(&self, page: &Page, key: &str) -> Result<()> {
        let dwell = self.dwell();
        self.key_press(page, key, dwell).await
    }

    async fn key_press(&self, page: &Page, key: &str, dwell_ms: f64) -> Result<()> {
        let keyboard = page.keyboard();
        self.guard("pressing a key", keyboard.down(key)).await?;
        self.sleep(dwell_ms).await;
        self.guard("releasing a key", keyboard.up(key)).await
    }

    fn dwell(&self) -> f64 {
        let t = self.traits();
        clamp(self.rnd().lognorm(t.dwell_mu, t.dwell_sigma), 30.0, 180.0)
    }

    /// Whether the field has the focus (no answer: `if_unknown`).
    pub(crate) async fn has_focus(&self, locator: &Locator, if_unknown: bool) -> bool {
        let check = locator.evaluate::<bool>(
            "el => el === document.activeElement || el.contains(document.activeElement)",
        );
        match tokio::time::timeout(Duration::from_secs(3), check).await {
            Ok(Ok(v)) => v,
            _ => if_unknown,
        }
    }

    /// Whether the field already has the text caret.
    pub async fn field_has_focus(&self, locator: &Locator) -> bool {
        self.has_focus(locator, false).await
    }

    /// Put the focus in the field with a human click. If it already has it:
    /// focused by our click a moment ago, type at once; focused by the page
    /// (autofocus), type at once or click anyway out of habit
    /// ([`Traits::clicks_when_focused`](super::Traits)). If the click does not
    /// leave the focus there, falls back to a direct click and then `focus()`.
    pub async fn focus_field(&self, locator: &Locator, timeout_ms: u64) -> Result<()> {
        let page = page_of(locator)?;
        if self.has_focus(locator, false).await {
            if self.since_last_click().is_some_and(|d| d < OUR_CLICK_FOCUS) {
                return Ok(());
            }
            if !self.traits().clicks_when_focused {
                self.count_variation("autofocus_typed");
                return Ok(());
            }
            self.count_variation("autofocus_clicked");
        }
        if self.cfg().human_mouse {
            match self.click_with_timeout(locator, timeout_ms.min(5000)).await {
                Ok(()) => {
                    if self.has_focus(locator, true).await {
                        return Ok(());
                    }
                    // A second click at once in the same place would be a
                    // double click (selecting the text): wait longer than that.
                    self.sleep(self.rnd().uniform(550.0, 800.0)).await;
                }
                Err(e) if e.is_timeout() && e.message().contains("did not respond") => {
                    // The browser does not answer mouse events: focus through the DOM.
                    tracing::debug!("{e}: focusing the field directly");
                    let _ = locator.focus().timeout(5000).await;
                    if self.has_focus(locator, false).await {
                        return Ok(());
                    }
                    return Err(e);
                }
                Err(_) => {}
            }
        }
        let b = locator.bounding_box().await.ok().flatten();
        locator.click().timeout(timeout_ms).force(true).await?;
        if let Some(b) = b {
            self.set_mouse_position(&page, b.x + b.width / 2.0, b.y + b.height / 2.0);
        }
        if !self.has_focus(locator, true).await {
            let _ = locator.focus().timeout(timeout_ms).await;
        }
        Ok(())
    }

    /// Type `text` into the field like a person; see [`TypeText`] for the options.
    pub fn type_text(&self, locator: &Locator, text: impl Into<String>) -> TypeText {
        TypeText {
            human: self.clone(),
            locator: locator.clone(),
            text: text.into(),
            speed: Speed::Normal,
            timeout_ms: 25_000,
            errors: true,
            focused: false,
            groups: Vec::new(),
            verify: false,
        }
    }

    /// Type a typo and correct it: 81% notice later (type a few more keys,
    /// stop, delete them all), 19% at once. Returns the extra keys typed.
    async fn typo(
        &self,
        w: &mut Writer<'_>,
        bad: char,
        rest: &[char],
        alpha: f64,
        beta: f64,
    ) -> Result<()> {
        let rnd = self.rnd();
        let conscious = rnd.random() > 0.19;
        w.character(bad, self.dwell()).await?;
        self.count_variation("typos");
        if !conscious {
            self.sleep(rnd.uniform(0.0, 600.0)).await;
            w.special("Backspace", rnd.uniform(40.0, 90.0)).await?;
            self.sleep(rnd.uniform(40.0, 150.0)).await;
            return Ok(());
        }
        let extra = (rnd.randint(2, 5) as usize).min(rest.len());
        for c in &rest[..extra] {
            self.sleep(clamp(rnd.log_logistic(alpha, beta), 25.0, 300.0))
                .await;
            w.character(*c, self.dwell()).await?;
        }
        self.sleep(rnd.uniform(500.0, 2000.0)).await;
        for _ in 0..=extra {
            w.special("Backspace", rnd.uniform(40.0, 90.0)).await?;
            self.sleep(rnd.uniform(30.0, 80.0)).await;
        }
        self.sleep(rnd.uniform(50.0, 150.0)).await;
        Ok(())
    }

    pub(super) async fn run_type(&self, t: &TypeText) -> Result<()> {
        let page = page_of(&t.locator)?;
        let cfg = self.cfg();
        if !cfg.human_typing {
            if !t.focused {
                self.focus_field(&t.locator, t.timeout_ms).await?;
            }
            return page.keyboard().r#type(&t.text).delay(100).await;
        }
        if !t.focused {
            self.focus_field(&t.locator, t.timeout_ms).await?;
            self.sleep(self.delay_s(0.15, 0.45) * 1000.0).await;
        }
        let mut w = Writer {
            human: self,
            page: &page,
            session: self.session(&page).await?,
            layout: self.keyboard_layout(&page).await,
            shift: None,
        };
        let result = self.type_chars(&mut w, t).await;
        // Always release Shift, even after a failure: a Shift left down
        // would spoil the next typing on this page.
        let released = w.release_shift().await;
        result.and(released)
    }

    /// The time (ms) between key `i - 1` and key `i` of `text`: log-logistic,
    /// adjusted by the letter pair, the hands, warm-up and fatigue, a pause
    /// after a space, now and then a hesitation, and the pauses between
    /// blocks (`groups`: the block starts) or, without blocks, at the
    /// boundaries of the content.
    pub(crate) fn flight_ms(
        &self,
        traits: &Traits,
        text: &[char],
        i: usize,
        (alpha, beta): (f64, f64),
        groups: Option<&HashSet<usize>>,
    ) -> f64 {
        let rnd = self.rnd();
        let (prev, ch) = (text[i - 1], text[i]);
        let mut fl = rnd.log_logistic(alpha, beta);
        let pair: String = prev.to_lowercase().chain(ch.to_lowercase()).collect();
        fl *= digraph(&pair);
        fl *= hand_multiplier(rnd, prev, ch);
        if i < traits.warmup_chars {
            fl *= traits.warmup_factor;
        }
        fl *= 1.0 + i as f64 * traits.fatigue_rate;
        if prev == ' ' && ch != ' ' {
            fl += rnd.uniform(25.0, 55.0);
        }
        fl = clamp(fl, 25.0, 900.0);
        if rnd.random() < traits.hesitation_chance {
            fl += rnd.uniform(180.0, 650.0);
        }
        if let Some(boundaries) = groups {
            // Next block (day → month, a phone group): a longer pause.
            if boundaries.contains(&i) {
                fl += rnd.uniform(220.0, 600.0);
            }
        } else if let Some((lo, hi)) = boundary_bump(prev, ch) {
            fl += rnd.uniform(lo, hi);
        } else if is_digit(prev) && is_digit(ch) {
            // A long number (card, phone): a pause every ~4 digits.
            let back = text[..i].iter().rev().take_while(|c| is_digit(**c)).count();
            let ahead = text[i..].iter().take_while(|c| is_digit(**c)).count();
            if back + ahead >= 8 && back % 4 == 0 {
                fl += rnd.uniform(110.0, 280.0);
            }
        }
        fl
    }

    /// The parameters (alpha, beta) of the time between keys at `speed`
    /// for this person, and the chance of a typo per key.
    pub(crate) fn rhythm(&self, traits: &Traits, speed: Speed) -> ((f64, f64), f64) {
        let (mut alpha, mut beta) = speed.params();
        alpha *= traits.typing_rhythm;
        beta = (beta + traits.flight_sigma_offset).max(2.0);
        // Slow typists make a few more mistakes.
        let error_rate = self.rnd().uniform(0.015, 0.04) * clamp(traits.typing_rhythm, 0.8, 1.5);
        ((alpha, beta), error_rate)
    }

    async fn type_chars(&self, w: &mut Writer<'_>, t: &TypeText) -> Result<()> {
        let rnd = self.rnd();
        let traits = self.traits();
        let text: Vec<char> = t.text.chars().collect();
        let ((alpha, beta), error_rate) = self.rhythm(&traits, t.speed);
        let n = text.len();
        let boundaries = group_boundaries(&t.groups, n);
        let mut i = 0;
        while i < n {
            let ch = text[i];
            if i > 0 {
                let fl = self.flight_ms(
                    &traits,
                    &text,
                    i,
                    (alpha, beta),
                    (!t.groups.is_empty()).then_some(&boundaries),
                );
                self.sleep(fl).await;
            }

            let make_error = t.errors && i > 0 && i + 1 < n && rnd.random() < error_rate;
            if make_error {
                let roll = rnd.random();
                if roll < 0.70 {
                    if let Some(bad) = typo_char(self, ch) {
                        self.typo(w, bad, &text[i + 1..], alpha, beta).await?;
                        continue; // types `ch` again
                    }
                } else if roll < 0.85 && i + 1 < n {
                    // Transposition: types the next one first, notices, fixes.
                    let next = text[i + 1];
                    w.character(next, self.dwell()).await?;
                    w.character(ch, self.dwell()).await?;
                    self.count_variation("typos");
                    self.sleep(rnd.uniform(150.0, 400.0)).await;
                    for _ in 0..2 {
                        w.special("Backspace", rnd.uniform(40.0, 90.0)).await?;
                        self.sleep(rnd.uniform(30.0, 80.0)).await;
                    }
                    self.sleep(rnd.uniform(50.0, 150.0)).await;
                    w.character(ch, self.dwell()).await?;
                    self.sleep(clamp(rnd.log_logistic(alpha, beta) * 0.9, 25.0, 300.0))
                        .await;
                    w.character(next, self.dwell()).await?;
                    i += 2;
                    continue;
                }
            }
            w.character(ch, self.dwell()).await?;
            i += 1;
        }
        Ok(())
    }
}

fn typo_char(human: &Human, c: char) -> Option<char> {
    let lower = c.to_lowercase().next()?;
    let adj: Vec<char> = adjacent(lower).chars().collect();
    if adj.is_empty() {
        return None;
    }
    let t = *human.rnd().choice(&adj);
    Some(if is_upper(c) {
        t.to_uppercase().next().unwrap_or(t)
    } else {
        t
    })
}

/// Builder returned by [`Human::type_text`] and [`Human::type_verified`]; `.await` it.
#[derive(Clone)]
#[must_use = "builders do nothing unless awaited"]
pub struct TypeText {
    pub(super) human: Human,
    pub(super) locator: Locator,
    pub(super) text: String,
    pub(super) speed: Speed,
    pub(super) timeout_ms: u64,
    pub(super) errors: bool,
    pub(super) focused: bool,
    pub(super) groups: Vec<usize>,
    pub(super) verify: bool,
}

impl TypeText {
    /// How fast, relative to the person's rhythm (default normal).
    pub fn speed(mut self, speed: Speed) -> Self {
        self.speed = speed;
        self
    }

    /// Timeout (ms) for the field to be visible (default 25 s).
    pub fn timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    /// Make and correct typos like a person (default true).
    pub fn errors(mut self, errors: bool) -> Self {
        self.errors = errors;
        self
    }

    /// The field already has the focus (reached with Tab, say): type without clicking.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// For number fields typed in blocks, the block sizes: a pause between
    /// blocks. A date `[2, 2, 4]`, a phone `[3, 3, 3]`. Without groups, the
    /// pauses follow the content (letters/digits, `@`, punctuation, long numbers).
    pub fn groups(mut self, groups: impl Into<Vec<usize>>) -> Self {
        self.groups = groups.into();
        self
    }

    /// For fields that must be right (an email, a password): type without
    /// typos, read the value back and, if it is wrong, fix it in place (a
    /// visible field) or clear it and retype it carefully once (a password).
    /// Fails if it is still wrong. A field that already holds text is
    /// cleared first. [`Human::type_verified`] turns it on.
    pub fn verify(mut self, verify: bool) -> Self {
        self.verify = verify;
        self
    }
}

impl IntoFuture for TypeText {
    type Output = Result<()>;
    type IntoFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            let human = self.human.clone();
            if self.verify {
                human.run_verified(&self).await
            } else {
                human.run_type(&self).await
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_mark_block_starts_inside_the_text() {
        let b = group_boundaries(&[2, 2, 4], 8);
        assert_eq!(b, [2, 4].into_iter().collect());
        let b = group_boundaries(&[3, 3, 3], 9);
        assert_eq!(b, [3, 6].into_iter().collect());
        assert!(group_boundaries(&[4], 4).is_empty());
        // More groups than text: no boundary outside it.
        assert_eq!(group_boundaries(&[2, 2, 4], 3), [2].into_iter().collect());
    }

    #[test]
    fn shift_is_pressed_by_the_other_hand() {
        let rnd = Rnd::new(Some(5));
        let right_share = |ch: char| {
            let n = 20_000;
            let right = (0..n)
                .filter(|_| shift_key(&rnd, ch).0 == "ShiftRight")
                .count();
            right as f64 / n as f64
        };
        // Left-hand letters and digits: right Shift 80%.
        for ch in ['A', 'Q', 'Z', 'Ç', 'Ã'] {
            let r = right_share(ch);
            assert!((r - 0.8).abs() < 0.015, "{ch}: {r}");
        }
        // Right-hand letters and every shifted symbol: right Shift 10%.
        for ch in ['P', 'Ñ', '@', '!', '#', '$', '%', '~', '"', '?'] {
            let r = right_share(ch);
            assert!((r - 0.1).abs() < 0.015, "{ch}: {r}");
        }
    }

    #[test]
    fn boundaries_by_content() {
        assert_eq!(boundary_bump('o', '4'), Some((90.0, 240.0)));
        assert_eq!(boundary_bump('a', '@'), Some((120.0, 300.0)));
        assert_eq!(boundary_bump('a', '.'), Some((70.0, 200.0)));
        assert_eq!(boundary_bump('a', 'b'), None);
    }
}
