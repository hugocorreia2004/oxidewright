//! Human-like input: mouse paths, clicks, typing, pauses, reading and
//! scrolling that look like a person, on top of the Playwright API.
//!
//! This is not part of Playwright. A [`Human`] is one simulated person: it
//! draws its traits once (speed, typing rhythm, key hold times, hand tremor,
//! fatigue, scroll style...) and keeps them, so everything one `Human` does
//! looks like the same person. Use one `Human` per browser profile.
//!
//! ```no_run
//! # async fn run(page: oxidewright::Page) -> oxidewright::Result<()> {
//! use oxidewright::human::{Human, KeyboardLayout, PauseKind};
//!
//! let human = Human::new().layout(KeyboardLayout::Pt);
//! human.click(&page.locator("#email")).await?;
//! human.type_text(&page.locator("#email"), "ana@example.com").await?;
//! human.pause(PauseKind::AfterClick).await;
//! human.click(&page.locator("button#next")).await?;
//! # Ok(()) }
//! ```
//!
//! What it does:
//! - **Mouse**: curved paths with Perlin tremor, Fitts' law timing, a speed
//!   cap, ease in and out, occasional overshoot and correction; optionally on
//!   the physical pixel grid at one move per display frame
//!   ([`Human::grid`]). Clicks aim off-center, hold the button for a
//!   log-normal time and the hand drifts away afterwards.
//! - **Typing**: per-key hold and flight times (log-logistic), faster digraphs
//!   and hand alternation, warm-up, fatigue, hesitations, pauses at the
//!   boundaries of letters/digits/punctuation and between number groups,
//!   typos corrected like a person, and the real keys of a keyboard layout
//!   (Shift, AltGr, dead keys).
//! - **Pauses and reading**: pauses whose length depends on what just
//!   happened, with the mouse drifting meanwhile; reading a text with the
//!   mouse resting near the next target or loosely following lines.
//! - **Scrolling**: wheel pushes that glide and slow down, in gestures, with
//!   the person's own style; lists inside menus scrolled with the wheel.
//! - **Touch**: taps and swipes for mobile profiles ([`Human::mobile`]).

mod correction;
mod keyboard;
mod math;
mod mouse;
mod pauses;
mod reading;
mod touch;
mod typing;

use std::collections::{HashMap, HashSet};
use std::future::IntoFuture;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::{BrowserContext, CDPSession, Error, EventKind, Locator, Page, Result};

pub use correction::{CorrectionPlan, correction_plan};
pub use keyboard::KeyboardLayout;
pub use pauses::{PauseKind, ReadingKind, ScrollDirection, ScrollStyle};
pub use reading::{ReadStyle, ReadText};
pub use typing::{Speed, TypeText};

use math::Rnd;

/// The traits of one simulated person, drawn once.
#[derive(Clone, Debug, PartialEq)]
pub struct Traits {
    /// Multiplies most pauses (0.55-1.6, mostly 0.75-1.25).
    pub speed_factor: f64,
    /// Multiplies the time between keys (0.55-2.2, median 1).
    pub typing_rhythm: f64,
    /// Shifts how regular the typing is (the log-logistic beta).
    pub flight_sigma_offset: f64,
    /// Log-normal parameters of how long a key is held.
    pub dwell_mu: f64,
    /// See `dwell_mu`.
    pub dwell_sigma: f64,
    /// Amplitude of the hand tremor, px.
    pub tremor_sigma: f64,
    /// The first keys of a text are slower by this factor...
    pub warmup_factor: f64,
    /// ...for this many keys.
    pub warmup_chars: usize,
    /// How much slower each key gets as the text goes on.
    pub fatigue_rate: f64,
    /// Chance of a hesitation before a key.
    pub hesitation_chance: f64,
    /// Seed of this person's tremor noise.
    pub noise_seed: f64,
    /// Chance of moving to the next field with Tab instead of the mouse
    /// (for flows that read it).
    pub prob_tab: f64,
    /// Chance of submitting with Enter instead of the mouse (for flows that read it).
    pub prob_enter: f64,
    /// Clicks a field that already has the focus before typing, out of habit.
    pub clicks_when_focused: bool,
    /// Reads texts before acting (otherwise a glance).
    pub reads_texts: bool,
    /// How this person scrolls.
    pub scroll_style: ScrollStyle,
}

impl Traits {
    fn draw(rnd: &Rnd) -> Self {
        let mut speed_factor = rnd.uniform(0.75, 1.25);
        if rnd.random() < 0.12 {
            speed_factor = if rnd.random() < 0.5 {
                rnd.uniform(0.55, 0.75)
            } else {
                rnd.uniform(1.25, 1.6)
            };
        }
        let typing_rhythm = rnd.lognorm(0.0, 0.35).clamp(0.55, 2.2);
        Self {
            speed_factor,
            typing_rhythm,
            flight_sigma_offset: rnd.uniform(-0.4, 0.4),
            dwell_mu: 4.3 + rnd.uniform(-0.3, 0.3),
            dwell_sigma: 0.35 + rnd.uniform(-0.05, 0.05),
            tremor_sigma: rnd.uniform(0.3, 0.8),
            warmup_factor: rnd.uniform(1.15, 1.45),
            warmup_chars: rnd.randint(3, 7) as usize,
            fatigue_rate: rnd.uniform(0.0003, 0.0007),
            hesitation_chance: rnd.uniform(0.02, 0.05) * typing_rhythm.clamp(0.8, 1.6),
            noise_seed: rnd.uniform(0.0, 1000.0),
            prob_tab: if rnd.random() < 0.55 {
                rnd.uniform(0.55, 0.95)
            } else {
                rnd.uniform(0.0, 0.1)
            },
            prob_enter: if rnd.random() < 0.45 {
                rnd.uniform(0.6, 0.95)
            } else {
                rnd.uniform(0.0, 0.08)
            },
            clicks_when_focused: rnd.random() < 0.35,
            reads_texts: rnd.random() < 0.75,
            scroll_style: rnd.weighted(
                &[
                    ScrollStyle::Continuous,
                    ScrollStyle::Hiccups,
                    ScrollStyle::Flick,
                    ScrollStyle::Slow,
                ],
                &[30.0, 35.0, 15.0, 20.0],
            ),
        }
    }
}

#[derive(Clone, Debug)]
struct Config {
    /// Chosen by the caller; `None`: from the browser's language.
    layout: Option<KeyboardLayout>,
    /// The layout found from the browser's language (once).
    detected_layout: Option<KeyboardLayout>,
    human_mouse: bool,
    human_typing: bool,
    mobile: bool,
    grid: bool,
    dpr: f64,
    frame_ms: f64,
    screen: (f64, f64),
    pace: f64,
    input_timeout: Duration,
}

#[derive(Default)]
struct State {
    positions: HashMap<usize, (f64, f64)>,
    last_page: Option<Page>,
    last_click: Option<Instant>,
    sessions: HashMap<usize, CDPSession>,
    /// Pages whose close this person listens to.
    watched: HashSet<usize>,
    variations: Map<String, Value>,
}

struct Inner {
    rnd: Rnd,
    perm: Vec<usize>,
    traits: Mutex<Traits>,
    config: Mutex<Config>,
    state: Mutex<State>,
}

/// One simulated person (cheap to clone; clones share the person).
#[derive(Clone)]
pub struct Human {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Human {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Human")
            .field("traits", &self.traits())
            .finish_non_exhaustive()
    }
}

impl Default for Human {
    fn default() -> Self {
        Self::new()
    }
}

/// The frame of a 60 Hz display, ms.
const FRAME_60HZ: f64 = 1000.0 / 60.0;

impl Human {
    /// A new person with random traits.
    pub fn new() -> Self {
        Self::build(None)
    }

    /// A new person whose traits and randomness come from `seed` (reproducible).
    pub fn with_seed(seed: u64) -> Self {
        Self::build(Some(seed))
    }

    fn build(seed: Option<u64>) -> Self {
        let rnd = Rnd::new(seed);
        let perm = rnd.permutation();
        let traits = Traits::draw(&rnd);
        let mut variations = Map::new();
        let r2 = |v: f64| (v * 100.0).round() / 100.0;
        variations.insert("prob_tab".into(), r2(traits.prob_tab).into());
        variations.insert("prob_enter".into(), r2(traits.prob_enter).into());
        variations.insert(
            "clicks_when_focused".into(),
            traits.clicks_when_focused.into(),
        );
        variations.insert("reads_texts".into(), traits.reads_texts.into());
        variations.insert("scroll_style".into(), traits.scroll_style.as_str().into());
        variations.insert("typing_rhythm".into(), r2(traits.typing_rhythm).into());
        variations.insert("speed".into(), r2(traits.speed_factor).into());
        Self {
            inner: Arc::new(Inner {
                rnd,
                perm,
                traits: Mutex::new(traits),
                config: Mutex::new(Config {
                    layout: None,
                    detected_layout: None,
                    human_mouse: true,
                    human_typing: true,
                    mobile: false,
                    grid: false,
                    dpr: 1.0,
                    frame_ms: FRAME_60HZ,
                    screen: (1920.0, 1080.0),
                    pace: 1.0,
                    input_timeout: Duration::from_secs(10),
                }),
                state: Mutex::new(State {
                    variations,
                    ..State::default()
                }),
            }),
        }
    }

    /// This person's traits.
    pub fn traits(&self) -> Traits {
        self.inner.traits.lock().unwrap().clone()
    }

    /// Replace this person's traits.
    pub fn set_traits(&self, traits: Traits) {
        *self.inner.traits.lock().unwrap() = traits;
    }

    /// The keyboard layout to type with. Without it, the layout of the
    /// browser's language (`navigator.language`: `es-ES` → Spanish, `en-GB`
    /// → UK...; see [`KeyboardLayout::for_locale`]), read the first time
    /// this person types.
    pub fn layout(self, layout: KeyboardLayout) -> Self {
        self.config(|c| c.layout = Some(layout));
        self
    }

    /// The layout this person types with on `page`: the chosen one, or the
    /// one of the browser's language (read once, then kept).
    pub async fn keyboard_layout(&self, page: &Page) -> KeyboardLayout {
        let cfg = self.cfg();
        if let Some(layout) = cfg.layout.or(cfg.detected_layout) {
            return layout;
        }
        let Ok(language) = page.evaluate::<String>("() => navigator.language").await else {
            return KeyboardLayout::Us;
        };
        let layout = KeyboardLayout::for_locale(&language);
        tracing::debug!("keyboard layout {layout:?} from the browser language {language}");
        self.config(|c| c.detected_layout = Some(layout));
        self.mark_variation("keyboard_layout", format!("{layout:?}"));
        layout
    }

    /// Mouse on the physical pixel grid: every point is a multiple of
    /// `1 / device_pixel_ratio` and the mouse sends one move per display
    /// frame (`frame_ms`, the `requestAnimationFrame` interval of the
    /// profile), like a real mouse. Without it, points have 0.1 px and the
    /// moves follow the timer.
    pub fn grid(self, device_pixel_ratio: f64, frame_ms: f64) -> Self {
        self.config(|c| {
            c.grid = true;
            c.dpr = device_pixel_ratio;
            c.frame_ms = frame_ms;
        });
        self
    }

    /// A touch device: clicks become taps and scrolling becomes swipes.
    pub fn mobile(self, mobile: bool) -> Self {
        self.config(|c| c.mobile = mobile);
        self
    }

    /// The screen size, used to place the mouse the first time on a page
    /// (default 1920x1080).
    pub fn screen(self, width: f64, height: f64) -> Self {
        self.config(|c| c.screen = (width, height));
        self
    }

    /// `false`: plain clicks (no mouse movement), for comparison.
    pub fn human_mouse(self, on: bool) -> Self {
        self.config(|c| c.human_mouse = on);
        self
    }

    /// `false`: plain typing (100 ms per key), for comparison.
    pub fn human_typing(self, on: bool) -> Self {
        self.config(|c| c.human_typing = on);
        self
    }

    /// How long a single mouse or key event may take before it fails
    /// (default 10 s): those protocol calls have no timeout of their own.
    pub fn input_timeout(self, timeout: Duration) -> Self {
        self.config(|c| c.input_timeout = timeout);
        self
    }

    /// Stretch (> 1) or shrink (< 1) every human wait, e.g. to make a whole
    /// flow take a target time. Clamped to 0.3-4.
    pub fn set_pace(&self, pace: f64) {
        let pace = if pace.is_finite() {
            pace.clamp(0.3, 4.0)
        } else {
            1.0
        };
        self.config(|c| c.pace = pace);
    }

    /// The current pace (see [`Human::set_pace`]).
    pub fn pace(&self) -> f64 {
        self.cfg().pace
    }

    /// What this person drew and did, for analysis: its traits and counters
    /// such as the typos made, the corrections, the scroll styles used.
    pub fn variations(&self) -> Map<String, Value> {
        self.inner.state.lock().unwrap().variations.clone()
    }

    /// Record a value in [`Human::variations`].
    pub fn mark_variation(&self, key: &str, value: impl Into<Value>) {
        self.inner
            .state
            .lock()
            .unwrap()
            .variations
            .insert(key.to_string(), value.into());
    }

    /// Add one to a counter in [`Human::variations`].
    pub fn count_variation(&self, key: &str) {
        let mut state = self.inner.state.lock().unwrap();
        let n = state
            .variations
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        state.variations.insert(key.to_string(), (n + 1).into());
    }

    /// Where this person's mouse is on `page` (placed near an edge the first
    /// time). Each page has its own position, forgotten when the page closes.
    pub fn mouse_position(&self, page: &Page) -> (f64, f64) {
        let key = page_key(page);
        if let Some(p) = self.inner.state.lock().unwrap().positions.get(&key) {
            return *p;
        }
        let (w, h) = self.cfg().screen;
        let (vw, vh) = ((w * 0.95).floor(), (h * 0.85).floor());
        let rnd = &self.inner.rnd;
        let zone = rnd.weighted(&[0, 1, 2, 3], &[50.0, 20.0, 15.0, 15.0]);
        let int = |a: f64, b: f64| rnd.randint(a as i64, b as i64) as f64;
        let p = match zone {
            0 => (int(vw * 0.05, vw * 0.55), int(5.0, 50.0)),
            1 => (int(5.0, 50.0), int(vh * 0.15, vh * 0.6)),
            2 => (int(vw * 0.9, vw), int(vh * 0.15, vh * 0.6)),
            _ => (int(vw * 0.15, vw * 0.55), int(vh * 0.85, vh)),
        };
        self.inner.state.lock().unwrap().positions.insert(key, p);
        self.watch(page);
        p
    }

    // ---- internals ----

    fn rnd(&self) -> &Rnd {
        &self.inner.rnd
    }

    fn perm(&self) -> &[usize] {
        &self.inner.perm
    }

    fn cfg(&self) -> Config {
        self.inner.config.lock().unwrap().clone()
    }

    fn config(&self, f: impl FnOnce(&mut Config)) {
        f(&mut self.inner.config.lock().unwrap());
    }

    fn set_mouse_position(&self, page: &Page, x: f64, y: f64) {
        let mut state = self.inner.state.lock().unwrap();
        state.positions.insert(page_key(page), (x, y));
        state.last_page = Some(page.clone());
        drop(state);
        self.watch(page);
    }

    /// Forget a page when it closes: its mouse position and its protocol
    /// session (the person's traits stay). Registered once per page.
    fn watch(&self, page: &Page) {
        let key = page_key(page);
        if !self.inner.state.lock().unwrap().watched.insert(key) {
            return;
        }
        // Weak: the page's listener must not keep the person alive.
        let inner: Weak<Inner> = Arc::downgrade(&self.inner);
        page.once(EventKind::Close, move |_| {
            if let Some(inner) = inner.upgrade() {
                forget(&inner, key);
            }
        });
        // Closed before the listener was in place: forget it now.
        if page.is_closed() {
            forget(&self.inner, key);
        }
    }

    /// Forget what this person knows about `page` (its mouse position and
    /// its protocol session); done by itself when the page closes.
    pub fn forget_page(&self, page: &Page) {
        forget(&self.inner, page_key(page));
    }

    /// Forget every page (mouse positions, protocol sessions); the traits
    /// stay. Done by itself when the browser context closes.
    pub fn forget_all(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.positions.clear();
        state.sessions.clear();
        state.watched.clear();
        state.last_page = None;
    }

    /// How many pages this person remembers a mouse position for.
    #[doc(hidden)]
    pub fn remembered_pages(&self) -> usize {
        self.inner.state.lock().unwrap().positions.len()
    }

    fn last_page(&self) -> Option<Page> {
        self.inner
            .state
            .lock()
            .unwrap()
            .last_page
            .clone()
            .filter(|p| !p.is_closed())
    }

    fn note_click(&self) {
        self.inner.state.lock().unwrap().last_click = Some(Instant::now());
    }

    fn since_last_click(&self) -> Option<Duration> {
        self.inner
            .state
            .lock()
            .unwrap()
            .last_click
            .map(|t| t.elapsed())
    }

    /// A coordinate on the physical pixel grid (with [`Human::grid`]; as is without).
    fn on_grid(&self, v: f64) -> f64 {
        let c = self.cfg();
        if !c.grid {
            return v;
        }
        let d = if c.dpr.is_finite() && c.dpr > 0.0 {
            c.dpr
        } else {
            1.0
        };
        (v * d).round() / d
    }

    /// `base_min`-`base_max` seconds, scaled by the speed factor and jittered.
    fn delay_s(&self, base_min: f64, base_max: f64) -> f64 {
        let base = self.rnd().uniform(base_min, base_max);
        let jitter = self.rnd().triangular(0.85, 1.15, 1.0);
        base * self.traits().speed_factor * jitter
    }

    /// A human wait: stretched or shrunk by the pace.
    async fn sleep(&self, ms: f64) {
        let ms = ms * self.pace();
        if ms > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(ms / 1000.0)).await;
        }
    }

    /// A wait that the pace never changes.
    async fn sleep_plain(&self, ms: f64) {
        if ms > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(ms / 1000.0)).await;
        }
    }

    /// The input call `fut`, failing if the browser does not answer in time.
    async fn guard<T>(&self, what: &str, fut: impl IntoFuture<Output = Result<T>>) -> Result<T> {
        let timeout = self.cfg().input_timeout;
        match tokio::time::timeout(timeout, fut.into_future()).await {
            Ok(r) => r,
            Err(_) => Err(Error::timeout(format!(
                "{what}: the browser did not respond in {:.1} s",
                timeout.as_secs_f64()
            ))),
        }
    }

    /// The raw protocol session of `page` (one per page, reused), for keys and touches.
    async fn session(&self, page: &Page) -> Result<CDPSession> {
        let key = page_key(page);
        if let Some(s) = self.inner.state.lock().unwrap().sessions.get(&key) {
            if !s.is_closed() {
                return Ok(s.clone());
            }
        }
        let context = page.context().ok_or_else(Error::target_closed)?;
        let session = context.new_cdp_session(page).await?;
        self.inner
            .state
            .lock()
            .unwrap()
            .sessions
            .insert(key, session.clone());
        self.watch(page);
        Ok(session)
    }
}

impl BrowserContext {
    /// The simulated person using this browser context: created (with random
    /// traits) the first time it is asked for, the same one after that, and
    /// dropped when the context closes (the browser closing closes it too).
    /// Use [`BrowserContext::set_human`] first to choose its seed, layout...
    pub fn human(&self) -> Human {
        let mut slot = self.inner.human.lock().unwrap();
        if let Some(h) = slot.as_ref() {
            return h.clone();
        }
        let h = Human::new();
        if !self.is_closed() {
            *slot = Some(h.clone());
        }
        h
    }

    /// Make `human` the person using this context (before the first
    /// [`BrowserContext::human`], to configure it).
    pub fn set_human(&self, human: Human) {
        if !self.is_closed() {
            *self.inner.human.lock().unwrap() = Some(human);
        }
    }
}

impl Page {
    /// The simulated person using this page's browser context; see
    /// [`BrowserContext::human`].
    pub fn human(&self) -> Human {
        self.context().map_or_else(Human::new, |c| c.human())
    }
}

fn forget(inner: &Inner, key: usize) {
    let mut state = inner.state.lock().unwrap();
    state.positions.remove(&key);
    state.sessions.remove(&key);
    state.watched.remove(&key);
    if state.last_page.as_ref().is_some_and(|p| page_key(p) == key) {
        state.last_page = None;
    }
}

fn page_key(page: &Page) -> usize {
    Arc::as_ptr(&page.inner) as usize
}

fn page_of(locator: &Locator) -> Result<Page> {
    locator.page().ok_or_else(Error::target_closed)
}

/// Wait until `deadline` with ~1 ms precision: the OS timer of some systems
/// (Windows) wakes in steps of ~15.6 ms, too coarse for a mouse at 60 Hz. The
/// timer takes it close and the rest yields to the runtime in a loop.
async fn wait_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let left = deadline - now;
        if left > Duration::from_millis(20) {
            tokio::time::sleep(left - Duration::from_millis(17)).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
}
