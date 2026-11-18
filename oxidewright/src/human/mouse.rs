//! Mouse movement and clicks.

use std::time::{Duration, Instant};

use serde::Deserialize;

use super::math::{
    MAX_SPEED, PathStyle, Point, build_path, clamp, clamp_viewport, fitts_time, overshoot_point,
    round1, should_overshoot, smooth_profile,
};
use super::{FRAME_60HZ, Human, page_of, wait_until};
use crate::{BoundingBox, Locator, Page, Result, WaitForSelectorState};

/// Keeps an element this far from the viewport edges when scrolling to it.
const VIEW_MARGIN: f64 = 8.0;

impl Human {
    /// Move the mouse to `(x, y)` like a person (target about 50 px wide).
    pub async fn move_to(&self, page: &Page, x: f64, y: f64) -> Result<()> {
        self.move_to_target(page, x, y, 50.0).await
    }

    /// Move the mouse to `(x, y)`; `target_width` is the size of what is
    /// aimed at (Fitts' law: smaller and farther takes longer and overshoots
    /// more often).
    pub async fn move_to_target(
        &self,
        page: &Page,
        x: f64,
        y: f64,
        target_width: f64,
    ) -> Result<()> {
        let (mx, my) = self.mouse_position(page);
        let dist = (x - mx).hypot(y - my);
        let mouse = page.mouse();
        if dist < 3.0 {
            let (gx, gy) = (self.on_grid(x), self.on_grid(y));
            self.guard("moving the mouse", mouse.move_to(gx, gy))
                .await?;
            self.set_mouse_position(page, gx, gy);
            return Ok(());
        }

        let cfg = self.cfg();
        let traits = self.traits();
        let rnd = self.rnd();
        let mut mt_ms = fitts_time(rnd, dist, target_width);
        // Even the shortest move is not instant.
        mt_ms = mt_ms.max(rnd.uniform(180.0, 300.0));
        if dist / mt_ms * 1000.0 > MAX_SPEED {
            mt_ms = dist / MAX_SPEED * 1000.0;
        }
        let frame_ms = self.frame_ms();
        let n = if cfg.grid {
            clamp((mt_ms * cfg.pace / frame_ms).round(), 10.0, 600.0) as usize
        } else {
            clamp((mt_ms / 16.7).floor(), 10.0, 200.0) as usize
        };
        let grid_round = |v: f64| self.on_grid(v);
        let style = PathStyle {
            round: if cfg.grid { &grid_round } else { &round1 },
            // On the grid: a smooth bell of speed and the slow tremor of a
            // hand (~6 cycles per second whatever the display rate).
            profile: if cfg.grid {
                Some(smooth_profile as fn(f64) -> f64)
            } else {
                None
            },
            noise_step: if cfg.grid { frame_ms * 0.006 } else { 0.15 },
        };

        let mut correction_pause = 0.0;
        let path: Vec<Point> = if should_overshoot(rnd, dist, target_width) {
            let (ox, oy) = overshoot_point(rnd, x, y, dist);
            let mut path = build_path(
                rnd,
                self.perm(),
                (mx, my),
                (ox, oy),
                (n as f64 * 0.8) as usize,
                traits.tremor_sigma,
                traits.noise_seed,
                &style,
            );
            path.extend(build_path(
                rnd,
                self.perm(),
                (ox, oy),
                (x, y),
                ((n as f64 * 0.3) as usize).max(5),
                traits.tremor_sigma * 0.5,
                traits.noise_seed + 50.0,
                &style,
            ));
            correction_pause = rnd.uniform(0.08, 0.18);
            path
        } else {
            build_path(
                rnd,
                self.perm(),
                (mx, my),
                (x, y),
                n,
                traits.tremor_sigma,
                traits.noise_seed,
                &style,
            )
        };
        let Some(last) = path.last().copied() else {
            return Ok(());
        };
        let correction_index = if correction_pause > 0.0 {
            Some((path.len() as f64 * 0.7) as usize)
        } else {
            None
        };

        if cfg.grid {
            self.move_by_frames(
                page,
                &path,
                correction_index,
                correction_pause * 1000.0 * cfg.pace,
                frame_ms,
            )
            .await?;
            self.set_mouse_position(page, last.x, last.y);
            return Ok(());
        }

        let total_s = (mt_ms + correction_pause * 1000.0) / 1000.0;
        let base = total_s / path.len().max(1) as f64;
        for (i, p) in path.iter().enumerate() {
            self.guard("moving the mouse", mouse.move_to(p.x, p.y))
                .await?;
            let progress = i as f64 / (path.len().max(2) - 1) as f64;
            let speed = if progress < 0.12 {
                0.3 + progress / 0.12 * 0.7
            } else if progress > 0.82 {
                1.0 - (progress - 0.82) / 0.18 * 0.7
            } else {
                1.0
            };
            let d = base / speed.max(0.15) * rnd.uniform(0.8, 1.2);
            self.sleep(clamp(d, 0.001, 0.025) * 1000.0).await;
            if Some(i) == correction_index {
                self.sleep(correction_pause * 1000.0).await;
            }
        }
        self.set_mouse_position(page, x, y);
        Ok(())
    }

    /// The display frame (ms), only if plausible (30-240 Hz); else 60 Hz.
    fn frame_ms(&self) -> f64 {
        let f = self.cfg().frame_ms;
        if f.is_finite() && (4.0..=34.0).contains(&f) {
            f
        } else {
            FRAME_60HZ
        }
    }

    /// The path at one move per frame, on a grid of `frame_ms` counted from
    /// the start (so protocol delays do not add up). A point equal to the
    /// previous one is not sent (the browser would not fire an event). A late
    /// send moves the next one to the next grid frame, never in a burst.
    async fn move_by_frames(
        &self,
        page: &Page,
        path: &[Point],
        correction_index: Option<usize>,
        correction_pause_ms: f64,
        frame_ms: f64,
    ) -> Result<()> {
        let mouse = page.mouse();
        let t0 = Instant::now();
        let frame_d = Duration::from_secs_f64(frame_ms / 1000.0);
        let mut frame: u32 = 0;
        let mut last: Option<Point> = None;
        let mut sent = t0;
        for (i, p) in path.iter().enumerate() {
            if last != Some(*p) {
                sent = Instant::now();
                self.guard("moving the mouse", mouse.move_to(p.x, p.y))
                    .await?;
                last = Some(*p);
            }
            if i == path.len() - 1 {
                break;
            }
            let pause = if Some(i) == correction_index {
                Duration::from_secs_f64(correction_pause_ms / 1000.0)
            } else {
                Duration::ZERO
            };
            // The next grid frame at least half a frame after the last send.
            let from = Instant::now().max(sent + frame_d / 2) + pause;
            let since = from.duration_since(t0).as_secs_f64() * 1000.0;
            frame = (frame + 1).max((since / frame_ms).ceil() as u32);
            wait_until(t0 + frame_d * frame).await;
        }
        Ok(())
    }

    /// Click the element like a person: wait until visible, bring it into
    /// view with the wheel, move to an off-center point, check the box again
    /// (layout shifts), press and release with a human hold time, then let
    /// the hand drift away. On a mobile profile it is a tap.
    pub async fn click(&self, locator: &Locator) -> Result<()> {
        self.click_with_timeout(locator, 25_000).await
    }

    /// [`Human::click`] with a timeout (ms) for the element to be visible.
    pub async fn click_with_timeout(&self, locator: &Locator, timeout_ms: u64) -> Result<()> {
        let cfg = self.cfg();
        if !cfg.human_mouse {
            return locator.click().timeout(timeout_ms).await;
        }
        let page = page_of(locator)?;
        locator
            .wait_for()
            .state(WaitForSelectorState::Visible)
            .timeout(timeout_ms)
            .await?;
        // If the page had to scroll, settle before moving, so the scroll and
        // the movement do not merge into one fast drag.
        if self.bring_into_view(locator).await? {
            self.sleep(self.rnd().uniform(180.0, 420.0)).await;
        }
        let rnd = self.rnd();

        if cfg.mobile {
            let Some(b) = locator.bounding_box().await? else {
                return locator.click().timeout(timeout_ms).await;
            };
            let (x, y) = self.click_point(&b);
            self.tap(&page, x, y).await?;
            return Ok(());
        }

        let Some(b) = locator.bounding_box().await? else {
            return locator.click().timeout(timeout_ms).await;
        };
        let (x, y) = self.click_point(&b);
        self.move_to_target(&page, x, y, b.width.min(b.height))
            .await?;
        self.sleep(rnd.uniform(40.0, 180.0)).await;
        let fresh = locator.bounding_box().await?;
        if let Some(f) = &fresh {
            if !same_box(f, &b) {
                let (nx, ny) = self.click_point(f);
                self.move_to_target(&page, nx, ny, f.width.min(f.height))
                    .await?;
            }
        }
        let dwell = clamp(rnd.lognorm(4.5, 0.38), 40.0, 300.0);
        self.press_and_release(&page, dwell).await?;
        self.note_click();
        self.after_click(&page, Some(fresh.unwrap_or(b))).await;
        Ok(())
    }

    /// Click at the exact point `(x, y)` like a person (to put the text caret
    /// somewhere in a field, say). `target_width` is the size aimed at.
    pub async fn click_at(&self, page: &Page, x: f64, y: f64, target_width: f64) -> Result<()> {
        self.move_to_target(page, x, y, target_width).await?;
        self.sleep(self.rnd().uniform(60.0, 200.0)).await;
        let dwell = clamp(self.rnd().lognorm(4.5, 0.38), 40.0, 300.0);
        self.press_and_release(page, dwell).await?;
        self.note_click();
        self.after_click(page, None).await;
        Ok(())
    }

    /// A point inside the box, biased like a person aiming at the text, not
    /// the exact center.
    fn click_point(&self, b: &BoundingBox) -> (f64, f64) {
        let rnd = self.rnd();
        let offset = rnd.sign() * rnd.uniform(0.05, 0.18);
        let cx = b.x + b.width * clamp(0.45 + offset + rnd.gauss(0.0, 0.05), 0.12, 0.88);
        let cy = b.y + b.height * clamp(rnd.gauss(0.5, 0.14), 0.15, 0.85);
        (cx, cy)
    }

    /// Down, hold, up. If the press does not answer, release anyway before
    /// failing, or the button would stay pressed in the page.
    async fn press_and_release(&self, page: &Page, dwell_ms: f64) -> Result<()> {
        let mouse = page.mouse();
        if let Err(e) = self.guard("pressing the mouse button", mouse.down()).await {
            let _ = self.guard("releasing the mouse button", mouse.up()).await;
            return Err(e);
        }
        self.sleep(dwell_ms).await;
        self.guard("releasing the mouse button", mouse.up()).await
    }

    /// After a click the hand does not stay still: usually the mouse leaves
    /// a little (off what was clicked, mostly down or right), sometimes it is
    /// only a twitch, now and then it stays. Never fails.
    async fn after_click(&self, page: &Page, clicked: Option<BoundingBox>) {
        let cfg = self.cfg();
        if !cfg.human_mouse || cfg.mobile {
            return;
        }
        let rnd = self.rnd();
        let r = rnd.random();
        if r < 0.12 {
            return;
        }
        self.sleep(rnd.uniform(80.0, 350.0)).await;
        let (x, y) = self.mouse_position(page);
        let (nx, mut ny);
        if r < 0.4 {
            nx = x + rnd.gauss(0.0, 1.2);
            ny = y + rnd.gauss(0.0, 1.0);
        } else {
            let angle = rnd.uniform(-0.25 * std::f64::consts::PI, 0.8 * std::f64::consts::PI);
            let d = rnd.uniform(15.0, 120.0);
            nx = x + angle.cos() * d;
            ny = y + angle.sin() * d;
            if let Some(b) = clicked {
                if ny >= b.y && ny <= b.y + b.height && nx >= b.x && nx <= b.x + b.width {
                    ny = b.y + b.height + rnd.uniform(4.0, 30.0);
                }
            }
        }
        let (cx, cy) = clamp_viewport(nx, ny);
        let _ = self.move_to_target(page, cx, cy, 60.0).await;
    }

    /// Bring the element into view with the mouse wheel, like a person. Inside
    /// a menu, list or dialog the wheel would close it or scroll the page
    /// behind, so there it only scrolls an option list that has its own
    /// scroll, and otherwise jumps. Returns whether the element moved (the
    /// page scrolled).
    pub async fn bring_into_view(&self, locator: &Locator) -> Result<bool> {
        let page = page_of(locator)?;
        let y0 = locator.bounding_box().await.ok().flatten().map(|b| b.y);
        let rnd = self.rnd();
        if self.in_overlay(locator).await {
            let cfg = self.cfg();
            if !cfg.mobile
                && cfg.human_mouse
                && self.scroll_list(&page, locator).await? == Some(true)
            {
                return Ok(moved(locator, y0).await);
            }
            if locator
                .scroll_into_view_if_needed()
                .timeout(5000)
                .await
                .is_ok()
            {
                self.sleep(rnd.uniform(50.0, 200.0)).await;
            }
            return Ok(moved(locator, y0).await);
        }
        #[derive(Deserialize)]
        struct View {
            h: f64,
        }
        let view: Option<View> = page
            .evaluate("() => ({ h: window.innerHeight })")
            .await
            .ok();
        if let Some(view) = view.filter(|v| v.h > 0.0) {
            for _ in 0..4 {
                let Some(b) = locator.bounding_box().await.ok().flatten() else {
                    break;
                };
                if b.y >= VIEW_MARGIN && b.y + b.height <= view.h - VIEW_MARGIN {
                    return Ok(moved(locator, y0).await);
                }
                // Put the element's center between 30% and 60% of the screen height.
                let delta = b.y + b.height / 2.0 - view.h * rnd.uniform(0.3, 0.6);
                let direction = if delta > 0.0 {
                    super::ScrollDirection::Down
                } else {
                    super::ScrollDirection::Up
                };
                self.scroll(&page, delta.abs().min(3000.0), direction)
                    .await?;
                self.sleep(rnd.uniform(120.0, 350.0)).await;
                let after = locator.bounding_box().await.ok().flatten();
                match after {
                    Some(a) if (a.y - b.y).abs() >= 5.0 => {}
                    _ => break, // the wheel does not move it
                }
            }
        }
        if locator
            .scroll_into_view_if_needed()
            .timeout(5000)
            .await
            .is_ok()
        {
            self.sleep(rnd.uniform(50.0, 200.0)).await;
        }
        Ok(moved(locator, y0).await)
    }

    /// Whether the element is in a menu, list of options or dialog.
    async fn in_overlay(&self, locator: &Locator) -> bool {
        locator
            .evaluate::<String>(
                r#"el => el.closest('[role="menu"], [role="menuitem"], [role="listbox"], [role="option"], [role="dialog"], [role="alertdialog"], [aria-modal="true"], dialog') ? 'overlay' : 'page'"#,
            )
            .await
            .is_ok_and(|v| v == "overlay")
    }

    /// An option in a list with its own scroll: the mouse goes over the list
    /// and the wheel scrolls it notch by notch, looking where it is. `Some(true)`:
    /// the option is in view; `Some(false)`: the wheel did not bring it;
    /// `None`: not an option of a scrolling list.
    async fn scroll_list(&self, page: &Page, locator: &Locator) -> Result<Option<bool>> {
        #[derive(Deserialize)]
        struct Geo {
            lx: f64,
            ly: f64,
            lw: f64,
            lh: f64,
            ey: f64,
            eh: f64,
            top: f64,
        }
        const GEO: &str = r#"el => {
            if (!el.closest('[role="option"]') || !el.closest('[role="listbox"], [role="menu"]')) return null;
            let c = el.parentElement;
            while (c && c !== document.body) {
                const cs = getComputedStyle(c);
                if ((cs.overflowY === 'auto' || cs.overflowY === 'scroll') && c.scrollHeight > c.clientHeight + 2) break;
                c = c.parentElement;
            }
            if (!c || c === document.body) return null;
            const lr = c.getBoundingClientRect(); const er = el.getBoundingClientRect();
            return { lx: lr.left, ly: lr.top, lw: lr.width, lh: lr.height, ey: er.top, eh: er.height, top: c.scrollTop };
        }"#;
        let rnd = self.rnd();
        let mut top_before: Option<f64> = None;
        for round in 0..60 {
            let g: Option<Geo> = locator.evaluate(GEO).await.ok().flatten();
            let Some(g) = g else {
                return Ok(if round == 0 { None } else { Some(false) });
            };
            let above = g.ey < g.ly + 2.0;
            let below = g.ey + g.eh > g.ly + g.lh - 2.0;
            if !above && !below {
                return Ok(Some(true));
            }
            if top_before.is_some_and(|t| (g.top - t).abs() < 1.0) {
                return Ok(Some(false));
            }
            top_before = Some(g.top);
            if round == 0 {
                // The hand goes over the list (the option is not visible).
                self.move_to_target(
                    page,
                    g.lx + g.lw * rnd.uniform(0.3, 0.75),
                    g.ly + g.lh * rnd.uniform(0.3, 0.7),
                    g.lw,
                )
                .await?;
                self.sleep(rnd.uniform(150.0, 400.0)).await;
            }
            let sign = if below { 1.0 } else { -1.0 };
            let left = if below {
                g.ey + g.eh - (g.ly + g.lh)
            } else {
                g.ly - g.ey
            };
            // Far: a gesture of 2-4 pushes; near: a small one, then look.
            let pushes = if left > 250.0 { rnd.randint(2, 4) } else { 1 };
            for k in 0..pushes {
                let px = if left > 250.0 {
                    rnd.uniform(70.0, 130.0)
                } else {
                    (left + rnd.uniform(15.0, 45.0)).min(rnd.uniform(50.0, 110.0))
                };
                self.wheel_push(page, sign * px, rnd.uniform(90.0, 170.0))
                    .await?;
                if k < pushes - 1 {
                    self.sleep(rnd.uniform(40.0, 130.0)).await;
                }
            }
            self.sleep(rnd.uniform(200.0, 600.0)).await;
        }
        Ok(Some(false))
    }
}

fn same_box(a: &BoundingBox, b: &BoundingBox) -> bool {
    a.x == b.x && a.y == b.y && a.width == b.width && a.height == b.height
}

async fn moved(locator: &Locator, y0: Option<f64>) -> bool {
    let Some(y0) = y0 else { return false };
    locator
        .bounding_box()
        .await
        .ok()
        .flatten()
        .is_some_and(|b| (b.y - y0).abs() > 5.0)
}
