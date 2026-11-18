//! Touches for mobile profiles: a click is a tap (touch start, a short hold,
//! touch end) and scrolling is a swipe (touch moves that slow down), sent
//! with `Input.dispatchTouchEvent`.

use serde::Deserialize;
use serde_json::{Value, json};

use super::math::clamp;
use super::{Human, ScrollDirection};
use crate::{Page, Result};

impl Human {
    /// A finger is wider and less regular than a cursor.
    fn finger(&self, x: f64, y: f64) -> Value {
        let rnd = self.rnd();
        json!({
            "x": x,
            "y": y,
            "id": 0,
            "radiusX": rnd.uniform(6.0, 14.0),
            "radiusY": rnd.uniform(6.0, 14.0),
            "force": rnd.uniform(0.6, 1.0),
        })
    }

    async fn touch(&self, page: &Page, kind: &str, points: Vec<Value>) -> Result<()> {
        let session = self.session(page).await?;
        self.guard(
            "sending a touch",
            session.send(
                "Input.dispatchTouchEvent",
                json!({ "type": kind, "touchPoints": points }),
            ),
        )
        .await
        .map(|_| ())
    }

    /// Tap at `(x, y)`: the finger lands a little off, stays a log-normal
    /// time, often slides a hair, and lifts.
    pub async fn tap(&self, page: &Page, x: f64, y: f64) -> Result<()> {
        let rnd = self.rnd();
        let (jx, jy) = (x + rnd.gauss(0.0, 1.5), y + rnd.gauss(0.0, 1.5));
        self.touch(page, "touchStart", vec![self.finger(jx, jy)])
            .await?;
        self.sleep(clamp(rnd.lognorm(4.6, 0.4), 45.0, 240.0)).await;
        if rnd.random() < 0.6 {
            let p = self.finger(jx + rnd.gauss(0.0, 1.2), jy + rnd.gauss(0.0, 1.2));
            self.touch(page, "touchMove", vec![p]).await?;
        }
        self.touch(page, "touchEnd", vec![]).await?;
        self.set_mouse_position(page, jx, jy);
        self.note_click();
        Ok(())
    }

    /// Swipe to scroll `distance` px; scrolling down means the finger moves
    /// up. The moves slow down at the end (momentum).
    pub async fn swipe(
        &self,
        page: &Page,
        distance: f64,
        direction: ScrollDirection,
    ) -> Result<()> {
        #[derive(Deserialize)]
        struct View {
            w: f64,
            h: f64,
        }
        let view: View = page
            .evaluate("() => ({ w: window.innerWidth, h: window.innerHeight })")
            .await
            .unwrap_or(View { w: 400.0, h: 800.0 });
        let rnd = self.rnd();
        let finger_up = direction == ScrollDirection::Down;
        let dist = distance.abs().min(view.h * 0.7);
        let x = view.w * rnd.uniform(0.35, 0.65);
        let y0 = if finger_up {
            view.h * rnd.uniform(0.6, 0.8)
        } else {
            view.h * rnd.uniform(0.2, 0.4)
        };
        let y1 = if finger_up { y0 - dist } else { y0 + dist };
        self.touch(page, "touchStart", vec![self.finger(x, y0)])
            .await?;
        let steps = rnd.randint(6, 12);
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let ease = 1.0 - (1.0 - t).powi(2);
            let y = y0 + (y1 - y0) * ease;
            let p = self.finger(x + rnd.gauss(0.0, 2.0), y);
            self.touch(page, "touchMove", vec![p]).await?;
            self.sleep(rnd.uniform(8.0, 18.0)).await;
        }
        self.touch(page, "touchEnd", vec![]).await?;
        self.set_mouse_position(page, x, y1);
        Ok(())
    }
}
