//! Pauses, reading and scrolling. Pauses do not freeze the cursor: it drifts
//! the way a resting hand does.

use std::future::IntoFuture;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::math::{clamp_viewport, pnoise1};
use super::{Human, wait_until};
use crate::{Page, Result};

/// What a pause follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseKind {
    /// After a page loaded.
    PageLoad,
    /// After a click.
    AfterClick,
    /// After submitting a form.
    AfterSubmit,
    /// After a dialog opened or closed.
    AfterModal,
    /// Finding one's way on a new screen.
    Orientation,
    /// A very short pause.
    Micro,
    /// Between two checks of something that is awaited.
    Polling,
    /// Anything else.
    General,
}

/// What is being read before acting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingKind {
    /// Before typing a password.
    BeforePassword,
    /// Before typing a name.
    BeforeName,
    /// Between related fields (often skipped).
    BetweenRelated,
    /// Before an address.
    BeforeAddress,
    /// Before a button or a decision.
    BeforeAction,
    /// Looking over a new page.
    ScanningPage,
    /// After an error message.
    AfterError,
    /// A cookie banner.
    CookieConsent,
    /// Anything else.
    Default,
}

/// Scroll direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollDirection {
    /// See what is below.
    Down,
    /// See what is above.
    Up,
}

/// How a person scrolls with the wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollStyle {
    /// Several pushes per gesture, short gaps.
    Continuous,
    /// One or two pushes, then a stop.
    Hiccups,
    /// Big fast pushes that go past and come back.
    Flick,
    /// Small pushes with long gaps.
    Slow,
}

impl ScrollStyle {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ScrollStyle::Continuous => "continuous",
            ScrollStyle::Hiccups => "hiccups",
            ScrollStyle::Flick => "flick",
            ScrollStyle::Slow => "slow",
        }
    }

    /// (push px, push ms, gap inside a gesture, pushes per gesture, gap between gestures).
    #[allow(clippy::type_complexity)]
    fn pattern(self) -> ((f64, f64), (f64, f64), (f64, f64), (i64, i64), (f64, f64)) {
        match self {
            ScrollStyle::Continuous => (
                (90.0, 170.0),
                (90.0, 160.0),
                (30.0, 110.0),
                (3, 6),
                (150.0, 450.0),
            ),
            ScrollStyle::Hiccups => (
                (70.0, 140.0),
                (90.0, 170.0),
                (40.0, 120.0),
                (1, 2),
                (350.0, 1300.0),
            ),
            ScrollStyle::Flick => (
                (180.0, 320.0),
                (140.0, 260.0),
                (40.0, 100.0),
                (1, 2),
                (250.0, 700.0),
            ),
            ScrollStyle::Slow => (
                (40.0, 90.0),
                (120.0, 220.0),
                (120.0, 320.0),
                (2, 4),
                (300.0, 900.0),
            ),
        }
    }
}

impl Human {
    /// The drift on the pixel grid: the hand is still most of the time;
    /// sometimes a small adjustment, rarely a larger reposition, always with
    /// the human movement. A busy page does not hold the pause longer.
    async fn grid_drift(&self, page: &Page, duration_s: f64) -> Result<()> {
        let rnd = self.rnd();
        let mut left = duration_s * 1000.0;
        let real_end = Instant::now() + Duration::from_secs_f64(duration_s + 2.0);
        while left > 0.0 {
            let wait = left.min(rnd.uniform(500.0, 2200.0));
            left -= wait;
            self.sleep(wait).await;
            if left <= 0.0 {
                break;
            }
            let r = rnd.random();
            if r >= 0.45 {
                continue; // the hand stays still
            }
            let (x, y) = self.mouse_position(page);
            let (nx, ny) = if r < 0.38 {
                clamp_viewport(x + rnd.gauss(0.0, 5.0), y + rnd.gauss(0.0, 3.0))
            } else {
                clamp_viewport(x + rnd.gauss(0.0, 35.0), y + rnd.gauss(0.0, 20.0))
            };
            let budget = real_end.saturating_duration_since(Instant::now());
            match tokio::time::timeout(budget, self.move_to_target(page, nx, ny, 40.0)).await {
                Err(_) => return Ok(()), // busy: the pause lasted what it should
                Ok(r) => r?,
            }
            left -= 250.0;
        }
        Ok(())
    }

    /// Drift the mouse smoothly for `duration_s`. Fails if the page closed.
    async fn drift(&self, page: &Page, duration_s: f64) -> Result<()> {
        let cfg = self.cfg();
        if cfg.mobile {
            // No cursor on a touch screen.
            self.sleep(duration_s * 1000.0).await;
            return Ok(());
        }
        if cfg.grid {
            return self.grid_drift(page, duration_s).await;
        }
        let rnd = self.rnd();
        let start = Instant::now();
        let end = start + Duration::from_secs_f64(duration_s);
        let offset = self.traits().noise_seed + rnd.uniform(0.0, 100.0);
        let mouse = page.mouse();
        while Instant::now() < end {
            let (mx, my) = self.mouse_position(page);
            let t = start.elapsed().as_secs_f64();
            let dx = pnoise1(self.perm(), t * 0.8 + offset, 2, 0.5) * 25.0;
            let dy = pnoise1(self.perm(), t * 0.6 + offset + 200.0, 2, 0.5) * 15.0;
            let (cx, cy) = clamp_viewport(mx + dx, my + dy);
            let (nx, ny) = (self.on_grid(cx), self.on_grid(cy));
            // The pause never waits for the mouse longer than it lasts.
            let budget = end.saturating_duration_since(Instant::now());
            match tokio::time::timeout(budget, mouse.move_to(nx, ny).into_future()).await {
                Err(_) => return Ok(()),
                Ok(r) => r?,
            }
            self.set_mouse_position(page, nx, ny);
            self.sleep(rnd.uniform(150.0, 800.0)).await;
        }
        Ok(())
    }

    /// A pause after `kind`, with the mouse drifting (on the last page this
    /// person used) when it lasts half a second or more. At most 15 s.
    pub async fn pause(&self, kind: PauseKind) {
        let rnd = self.rnd();
        let mut delay = match kind {
            PauseKind::PageLoad => rnd.lognorm(7.5, 0.4) / 1000.0,
            PauseKind::AfterClick => rnd.ex_gaussian(200.0, 50.0, 150.0) / 1000.0,
            PauseKind::AfterSubmit => rnd.lognorm(8.2, 0.5) / 1000.0,
            PauseKind::AfterModal => rnd.ex_gaussian(250.0, 60.0, 200.0) / 1000.0,
            PauseKind::Orientation => rnd.lognorm(7.0, 0.5) / 1000.0,
            PauseKind::Micro => rnd.uniform(0.15, 0.5),
            PauseKind::Polling => rnd.uniform(0.25, 0.6),
            PauseKind::General => rnd.lognorm(6.8, 0.5) / 1000.0,
        };
        delay *= self.traits().speed_factor;
        if kind == PauseKind::Polling {
            delay = delay.max(0.2);
        }
        delay = delay.min(15.0);
        if delay >= 0.5 {
            if let Some(page) = self.last_page() {
                if self.drift(&page, delay).await.is_ok() {
                    return;
                }
            }
        }
        self.sleep(delay * 1000.0).await;
    }

    /// Look at the screen before acting, with the mouse drifting.
    pub async fn read(&self, page: &Page, kind: ReadingKind) {
        // (duration s, with the mouse, chance of just thinking, chance of skipping)
        let (dur, mouse, think, skip) = match kind {
            ReadingKind::BeforePassword => ((2.0, 5.0), true, 0.5, 0.0),
            ReadingKind::BeforeName => ((0.4, 1.2), true, 0.0, 0.0),
            ReadingKind::BetweenRelated => ((0.15, 0.6), false, 0.0, 0.4),
            ReadingKind::BeforeAddress => ((2.5, 5.5), true, 0.6, 0.0),
            ReadingKind::BeforeAction => ((1.0, 3.5), true, 0.3, 0.0),
            ReadingKind::ScanningPage => ((1.5, 4.0), true, 0.0, 0.0),
            ReadingKind::AfterError => ((2.0, 4.0), true, 0.7, 0.0),
            ReadingKind::CookieConsent => ((0.8, 2.5), true, 0.05, 0.0),
            ReadingKind::Default => ((1.0, 3.0), true, 0.1, 0.0),
        };
        let rnd = self.rnd();
        if rnd.random() < skip {
            self.sleep(rnd.uniform(40.0, 120.0)).await;
            return;
        }
        if rnd.random() < think {
            self.sleep(self.delay_s(dur.0, dur.1) * 1000.0).await;
            return;
        }
        let s = self.delay_s(dur.0, dur.1);
        if mouse && self.drift(page, s).await.is_ok() {
            return;
        }
        self.sleep(s * 1000.0).await;
    }

    /// Think for `min_s`-`max_s` seconds (scaled by the person's speed), without moving.
    pub async fn think(&self, min_s: f64, max_s: f64) {
        self.sleep(self.delay_s(min_s, max_s) * 1000.0).await;
    }

    /// Now and then (`probability`) a distraction of 2-8 s.
    pub async fn maybe_distraction(&self, probability: f64) {
        let rnd = self.rnd();
        if rnd.random() > probability {
            return;
        }
        self.sleep(rnd.uniform(2.0, 8.0) * self.traits().speed_factor * 1000.0)
            .await;
    }

    /// One push of the wheel: `total` px in small events every 10-20 ms,
    /// slowing down at the end, over `duration_ms`, so the page glides like
    /// with a real wheel (one protocol event of 100 px is a jump).
    pub(crate) async fn wheel_push(&self, page: &Page, total: f64, duration_ms: f64) -> Result<()> {
        let rnd = self.rnd();
        let sign = if total < 0.0 { -1.0 } else { 1.0 };
        let px = total.abs().round().max(1.0);
        let steps = ((duration_ms / rnd.uniform(10.0, 20.0)).round() as usize).max(2);
        let weights: Vec<f64> = (0..steps)
            .map(|i| (1.0 - i as f64 / steps as f64).powf(1.6) * rnd.uniform(0.85, 1.15))
            .collect();
        let sum: f64 = weights.iter().sum();
        let (mut done, mut acc) = (0.0, 0.0);
        let t0 = Instant::now();
        let mouse = page.mouse();
        for (i, w) in weights.iter().enumerate() {
            acc += w;
            let upto = if i == steps - 1 {
                px
            } else {
                (px * acc / sum).round()
            };
            let delta = upto - done;
            if delta > 0.0 {
                self.guard("turning the mouse wheel", mouse.wheel(0.0, sign * delta))
                    .await?;
                done = upto;
            }
            wait_until(
                t0 + Duration::from_secs_f64((i + 1) as f64 * duration_ms / steps as f64 / 1000.0),
            )
            .await;
        }
        Ok(())
    }

    /// Scroll `distance` px with the wheel like a person: gliding pushes of
    /// sizes that never repeat, in gestures with gaps, in the person's own
    /// style (one time in four another one, for variety). On a mobile
    /// profile it is a swipe.
    pub async fn scroll(
        &self,
        page: &Page,
        distance: f64,
        direction: ScrollDirection,
    ) -> Result<()> {
        if self.cfg().mobile && self.swipe(page, distance, direction).await.is_ok() {
            return Ok(());
        }
        let rnd = self.rnd();
        let sign = if direction == ScrollDirection::Down {
            1.0
        } else {
            -1.0
        };
        let style = if rnd.random() < 0.75 {
            self.traits().scroll_style
        } else {
            *rnd.choice(&[
                ScrollStyle::Continuous,
                ScrollStyle::Hiccups,
                ScrollStyle::Flick,
                ScrollStyle::Slow,
            ])
        };
        self.count_variation(&format!("scroll_{}", style.as_str()));
        let (px_r, dur_r, inside_r, per_r, between_r) = style.pattern();
        let mut left = distance.max(20.0);
        while left > 0.0 {
            let pushes = rnd.randint(per_r.0, per_r.1);
            let mut k = 0;
            while k < pushes && left > 0.0 {
                // A flick goes past the target (corrected at the end); the others stop near it.
                let mut px = rnd.uniform(px_r.0, px_r.1).round();
                if style != ScrollStyle::Flick || px < left {
                    px = px.min(left.max(20.0));
                }
                self.wheel_push(page, sign * px, rnd.uniform(dur_r.0, dur_r.1))
                    .await?;
                left -= px;
                if k < pushes - 1 && left > 0.0 {
                    self.sleep(rnd.uniform(inside_r.0, inside_r.1)).await;
                }
                k += 1;
            }
            if left > 0.0 {
                self.sleep(rnd.uniform(between_r.0, between_r.1)).await;
            }
        }
        // Went past (a flick, or sometimes the others): a small push back.
        if left < -15.0 || (distance >= 200.0 && rnd.random() < 0.1) {
            self.sleep(rnd.uniform(200.0, 550.0)).await;
            let back = if left < 0.0 {
                -left
            } else {
                rnd.uniform(40.0, 90.0)
            };
            self.wheel_push(
                page,
                -sign * back.clamp(25.0, 160.0),
                rnd.uniform(90.0, 160.0),
            )
            .await?;
        }
        Ok(())
    }

    /// Explore the page with probability `probability`: scroll down in a few
    /// steps through 40-80% of it, then back up almost all the way.
    pub async fn explore(&self, page: &Page, probability: f64) -> Result<()> {
        let rnd = self.rnd();
        if rnd.random() > probability {
            self.sleep(rnd.uniform(500.0, 1500.0)).await;
            return Ok(());
        }
        #[derive(Deserialize)]
        struct Dims {
            sh: f64,
            ch: f64,
        }
        let dims: Option<Dims> = page
            .evaluate(
                "() => ({ sh: document.documentElement.scrollHeight, ch: window.innerHeight })",
            )
            .await
            .ok();
        let Some(dims) = dims else {
            let d = rnd.randint(150, 350) as f64;
            self.scroll(page, d, ScrollDirection::Down).await?;
            self.sleep(rnd.uniform(500.0, 1500.0)).await;
            return self
                .scroll(
                    page,
                    (d * rnd.uniform(0.85, 0.95)).floor(),
                    ScrollDirection::Up,
                )
                .await;
        };
        let max = dims.sh - dims.ch;
        if max <= 50.0 {
            self.sleep(rnd.uniform(800.0, 2000.0)).await;
            return Ok(());
        }
        let total = (max * rnd.uniform(0.4, 0.8)).floor();
        let n = rnd.randint(2, 4);
        let mut steps = Vec::new();
        let mut remaining = total;
        for i in 0..n {
            let step = if i == n - 1 {
                remaining
            } else {
                (remaining * rnd.uniform(0.2, 0.5)).floor()
            };
            steps.push(step.max(30.0));
            remaining -= step;
        }
        let mut down = 0.0;
        for step in steps {
            self.scroll(page, step, ScrollDirection::Down).await?;
            down += step;
            self.sleep(rnd.uniform(800.0, 2500.0)).await;
        }
        let up = (down * (1.0 - rnd.uniform(0.05, 0.15))).floor();
        if rnd.random() < 0.5 {
            self.scroll(page, up, ScrollDirection::Up).await
        } else {
            let first = (up * rnd.uniform(0.5, 0.7)).floor();
            self.scroll(page, first, ScrollDirection::Up).await?;
            self.sleep(rnd.uniform(300.0, 800.0)).await;
            self.scroll(page, up - first, ScrollDirection::Up).await
        }
    }

    /// Accept a consent banner like a person: wait for the button (up to
    /// `timeout_ms`), read a moment, click. Returns `false` if it did not appear.
    pub async fn accept_consent(&self, accept: &crate::Locator, timeout_ms: u64) -> Result<bool> {
        if accept
            .wait_for()
            .state(crate::WaitForSelectorState::Visible)
            .timeout(timeout_ms)
            .await
            .is_err()
        {
            return Ok(false);
        }
        let page = super::page_of(accept)?;
        self.read(&page, ReadingKind::CookieConsent).await;
        self.click_with_timeout(accept, timeout_ms).await?;
        Ok(true)
    }
}
