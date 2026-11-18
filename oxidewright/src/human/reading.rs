//! Reading a text before acting, with the mouse doing what a reader's does:
//! not tracing the lines with a ruler. Either it rests near where the next
//! action is (and only twitches now and then), or it loosely follows one to
//! three lines, stopping halfway, sometimes going back, and then rests.
//! Sometimes the way passes over a link of the text, never going back to it.
//! The time is the one of someone skimming (~25-45 ms per character,
//! 1.5-14 s). The lines and links come from the text itself
//! (`getClientRects` of an inline element: one box per line).

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Instant;

use serde::Deserialize;

use super::math::clamp;
use super::{Human, page_of};
use crate::{Locator, Page, Result};

/// How the mouse behaves while reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadStyle {
    /// Rests near the next target.
    Rest,
    /// Loosely follows some lines, then rests.
    Follow,
    /// Does not read: a glance.
    Skip,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Debug, Deserialize)]
struct TextGeometry {
    lines: Vec<Rect>,
    links: Vec<Rect>,
    chars: usize,
}

const GEOMETRY: &str = r#"el => ({
    lines: [...el.getClientRects()].filter(r => r.width > 4 && r.height > 4).map(r => ({ x: r.left, y: r.top, w: r.width, h: r.height })),
    links: [...el.querySelectorAll('a')].map(a => a.getBoundingClientRect()).filter(r => r.width > 4 && r.height > 4).map(r => ({ x: r.left, y: r.top, w: r.width, h: r.height })),
    chars: (el.textContent || '').trim().length,
})"#;

/// The reading time is counted by what is planned (each pause and move uses
/// part of the budget), not by the clock.
struct Budget(f64);

impl Budget {
    fn done(&self) -> bool {
        self.0 <= 0.0
    }

    async fn pause(&mut self, human: &Human, ms: f64) {
        let ms = ms.min(self.0.max(0.0));
        self.0 -= ms;
        human.sleep(ms).await;
    }

    fn movement(&mut self) {
        self.0 -= 250.0;
    }
}

impl Human {
    /// Read the text of `text` before acting; see [`ReadText`] for the options.
    pub fn read_text(&self, text: &Locator) -> ReadText {
        ReadText {
            human: self.clone(),
            text: text.clone(),
            target: None,
            style: None,
        }
    }

    /// Where the hand rests while reading: near the next target (without
    /// covering it), or at the start of the text, never below the last line.
    fn rest_point(&self, t: &TextGeometry, target: Option<Rect>) -> (f64, f64) {
        let rnd = self.rnd();
        let first = t.lines[0];
        let last = t.lines[t.lines.len() - 1];
        let zone = if target.is_some() {
            rnd.weighted(&[0, 1, 2], &[55.0, 30.0, 15.0])
        } else {
            rnd.weighted(&[1, 2], &[65.0, 35.0])
        };
        match (zone, target) {
            (0, Some(a)) => (
                a.x + a.w * rnd.uniform(1.1, 1.9),
                a.y + a.h * rnd.uniform(0.2, 0.9),
            ),
            (1, _) => (
                first.x + first.w * rnd.uniform(0.02, 0.25),
                first.y + first.h * rnd.uniform(0.6, 1.2),
            ),
            _ => {
                let l = t.lines[rnd.index(t.lines.len())];
                (
                    l.x + l.w * rnd.uniform(0.15, 0.6),
                    (l.y + l.h * rnd.uniform(0.5, 1.2)).min(last.y + last.h * 0.9),
                )
            }
        }
    }

    async fn go_rest(
        &self,
        page: &Page,
        t: &TextGeometry,
        target: Option<Rect>,
        b: &mut Budget,
    ) -> Result<()> {
        let rnd = self.rnd();
        if !t.links.is_empty() && rnd.random() < 0.15 {
            let a = t.links[rnd.index(t.links.len())];
            self.move_to_target(
                page,
                a.x + a.w * rnd.uniform(0.2, 0.8),
                a.y + a.h * rnd.uniform(0.3, 0.8),
                a.w,
            )
            .await?;
            b.movement();
            // Over the link, wondering whether to open it.
            b.pause(self, rnd.uniform(300.0, 900.0)).await;
        }
        let (px, py) = self.rest_point(t, target);
        self.move_to_target(page, px, py, 60.0).await?;
        b.movement();
        Ok(())
    }

    /// The hand resting on the mouse: still for seconds, now and then a
    /// 1-2 px twitch.
    async fn resting_hand(&self, page: &Page, b: &mut Budget) -> Result<()> {
        let rnd = self.rnd();
        while !b.done() {
            b.pause(self, rnd.uniform(600.0, 2500.0)).await;
            if b.done() {
                break;
            }
            if rnd.random() < 0.3 {
                let (x, y) = self.mouse_position(page);
                self.move_to_target(
                    page,
                    x + rnd.sign() * rnd.uniform(0.6, 1.8),
                    y + rnd.gauss(0.0, 0.8),
                    40.0,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Follows a few lines in jolts, imprecisely, stopping when it likes.
    async fn follow_lines(&self, page: &Page, t: &TextGeometry, b: &mut Budget) -> Result<()> {
        let rnd = self.rnd();
        let n = (rnd.randint(1, 3) as usize).min(t.lines.len());
        for l in &t.lines[..n] {
            if b.done() {
                break;
            }
            let y = l.y + l.h * rnd.uniform(0.5, 1.3);
            let mut x = l.x + rnd.uniform(-10.0, l.w * 0.15);
            self.move_to_target(page, x, y + rnd.gauss(0.0, 1.5), 60.0)
                .await?;
            b.movement();
            // Rarely to the end of the line.
            let until = l.x + l.w * rnd.uniform(0.45, 0.95);
            while x < until && !b.done() {
                // The eyes go ahead; the mouse waits.
                b.pause(self, rnd.uniform(180.0, 900.0)).await;
                x += rnd.uniform(25.0, 110.0);
                if rnd.random() < 0.12 {
                    x -= rnd.uniform(20.0, 60.0); // back to read again
                }
                self.move_to_target(page, x.min(until), y + rnd.gauss(0.0, 1.5), 60.0)
                    .await?;
                b.movement();
            }
        }
        Ok(())
    }

    async fn run_read(&self, r: &ReadText) -> Result<f64> {
        let start = Instant::now();
        let rnd = self.rnd();
        let style = r.style.unwrap_or(if self.traits().reads_texts {
            if rnd.random() < 0.65 {
                ReadStyle::Rest
            } else {
                ReadStyle::Follow
            }
        } else {
            ReadStyle::Skip
        });
        if style == ReadStyle::Skip {
            self.sleep(rnd.uniform(300.0, 1200.0)).await;
            self.count_variation("read_skip");
            return Ok(start.elapsed().as_secs_f64());
        }
        let page = page_of(&r.text)?;
        let geometry: Option<TextGeometry> = r.text.evaluate(GEOMETRY).await.ok();
        let total = clamp(
            geometry.as_ref().map_or(120, |g| g.chars) as f64 * rnd.uniform(25.0, 45.0),
            1500.0,
            14_000.0,
        );
        let cfg = self.cfg();
        let geometry = geometry.filter(|g| !g.lines.is_empty());
        let Some(geometry) = geometry.filter(|_| cfg.human_mouse && !cfg.mobile) else {
            self.sleep(total).await;
            return Ok(start.elapsed().as_secs_f64());
        };
        let target = match &r.target {
            Some(t) => t.bounding_box().await.ok().flatten().map(|b| Rect {
                x: b.x,
                y: b.y,
                w: b.width,
                h: b.height,
            }),
            None => None,
        };
        let mut budget = Budget(total);
        let run = async {
            if style == ReadStyle::Follow {
                self.follow_lines(&page, &geometry, &mut budget).await?;
            }
            self.go_rest(&page, &geometry, target, &mut budget).await?;
            self.resting_hand(&page, &mut budget).await
        };
        // The mouse not answering ends the reading; the flow goes on.
        let _ = run.await;
        self.count_variation(if style == ReadStyle::Follow {
            "read_follow"
        } else {
            "read_rest"
        });
        Ok(start.elapsed().as_secs_f64())
    }
}

/// Builder returned by [`Human::read_text`]; `.await` it for the seconds it took.
#[must_use = "builders do nothing unless awaited"]
pub struct ReadText {
    human: Human,
    text: Locator,
    target: Option<Locator>,
    style: Option<ReadStyle>,
}

impl ReadText {
    /// Where the next action is (a checkbox, a button): the hand usually rests near it.
    pub fn target(mut self, target: &Locator) -> Self {
        self.target = Some(target.clone());
        self
    }

    /// Force a reading style (otherwise drawn, from the person's traits).
    pub fn style(mut self, style: ReadStyle) -> Self {
        self.style = Some(style);
        self
    }
}

impl IntoFuture for ReadText {
    type Output = Result<f64>;
    type IntoFuture = Pin<Box<dyn Future<Output = Result<f64>> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { self.human.clone().run_read(&self).await })
    }
}
