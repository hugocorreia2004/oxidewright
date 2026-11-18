//! Correcting a text field like a person: click where the mistake is, delete
//! only what is wrong and type only what is missing, instead of clearing and
//! retyping everything. Everything read in the page is read only (the text
//! width is measured on a detached canvas).

use serde::Deserialize;

use super::math::clamp;
use super::{Human, Speed, page_of};
use crate::{Error, Locator, Result};

/// What changes `current` into `expected`: where to put the caret, how many
/// characters to delete backwards from there, and what to type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrectionPlan {
    /// Caret position (characters).
    pub caret: usize,
    /// Backspaces from the caret.
    pub delete: usize,
    /// Text to type after deleting.
    pub insert: String,
}

/// The minimal plan: the common prefix and suffix stay, the different middle
/// is deleted and the right text typed in its place. `"Lopez"` → `"López"`:
/// the middle is `"o"` vs `"ó"`, so caret 2 (after the `"o"`), delete 1,
/// type `"ó"`.
pub fn correction_plan(current: &str, expected: &str) -> CorrectionPlan {
    let a: Vec<char> = current.chars().collect();
    let b: Vec<char> = expected.chars().collect();
    let mut p = 0;
    while p < a.len() && p < b.len() && a[p] == b[p] {
        p += 1;
    }
    let mut s = 0;
    while s < a.len() - p && s < b.len() - p && a[a.len() - 1 - s] == b[b.len() - 1 - s] {
        s += 1;
    }
    CorrectionPlan {
        caret: a.len() - s,
        delete: a.len() - s - p,
        insert: b[p..b.len() - s].iter().collect(),
    }
}

#[derive(Deserialize)]
struct Geometry {
    x: f64,
    y: f64,
    #[serde(rename = "xMin")]
    x_min: f64,
    #[serde(rename = "xMax")]
    x_max: f64,
    len: usize,
}

const GEOMETRY: &str = r#"(el, i) => {
    if (typeof el.value !== 'string') return null;
    const cs = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    const c = document.createElement('canvas').getContext('2d');
    if (!c) return null;
    c.font = cs.font || `${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`;
    const spacing = parseFloat(cs.letterSpacing) || 0;
    const upTo = [...el.value].slice(0, i).join('');
    const width = c.measureText(upTo).width + spacing * [...upTo].length;
    const left = r.left + (parseFloat(cs.borderLeftWidth) || 0) + (parseFloat(cs.paddingLeft) || 0);
    const right = r.right - (parseFloat(cs.borderRightWidth) || 0) - (parseFloat(cs.paddingRight) || 0);
    const top = r.top + (parseFloat(cs.borderTopWidth) || 0) + (parseFloat(cs.paddingTop) || 0);
    const bottom = r.bottom - (parseFloat(cs.borderBottomWidth) || 0) - (parseFloat(cs.paddingBottom) || 0);
    return { x: left + width - el.scrollLeft, y: (top + bottom) / 2, xMin: left, xMax: right, len: [...el.value].length };
}"#;

/// The caret position in characters (the page counts UTF-16 units).
const CARET: &str = r#"el => {
    const s = el.selectionStart;
    if (s === null || s === undefined) return null;
    return [...el.value.slice(0, s)].length;
}"#;

impl Human {
    async fn caret(&self, field: &Locator) -> Option<usize> {
        field.evaluate::<Option<usize>>(CARET).await.ok().flatten()
    }

    /// Put the text caret at character `index` with a click there (at the
    /// end: a click in the empty space right of the text). If the click lands
    /// a character or two off, fixes it with the arrows. Returns whether the
    /// caret is there.
    pub async fn put_caret(&self, field: &Locator, index: usize) -> Result<bool> {
        let page = page_of(field)?;
        let Some(g) = field
            .evaluate::<Option<Geometry>>(GEOMETRY)
            .arg(index)
            .await
            .ok()
            .flatten()
        else {
            return Ok(false);
        };
        let rnd = self.rnd();
        let at_end = index >= g.len;
        let x = if at_end {
            clamp(g.x + rnd.uniform(4.0, 28.0), g.x_min + 1.0, g.x_max - 2.0)
        } else {
            clamp(g.x + rnd.gauss(0.0, 0.6), g.x_min + 1.0, g.x_max - 2.0)
        };
        self.click_at(
            &page,
            x,
            g.y + rnd.gauss(0.0, 1.5),
            if at_end { 20.0 } else { 6.0 },
        )
        .await?;
        let mut caret = self.caret(field).await;
        if caret.is_none() {
            return Ok(false);
        }
        if at_end && caret != Some(index) {
            self.press(&page, "End").await?;
            caret = self.caret(field).await;
        }
        let mut n = 0;
        while let Some(c) = caret {
            if c == index || n >= 6 {
                break;
            }
            self.sleep(rnd.uniform(60.0, 160.0)).await;
            self.press(&page, if c > index { "ArrowLeft" } else { "ArrowRight" })
                .await?;
            caret = self.caret(field).await;
            n += 1;
        }
        Ok(caret == Some(index))
    }

    /// Correct the field in place until it holds `expected` (two tries).
    /// Returns whether it does; `false` when the caret cannot be placed (the
    /// caller decides the fallback, such as clearing and retyping).
    pub async fn fix_field(&self, field: &Locator, expected: &str) -> Result<bool> {
        let page = page_of(field)?;
        let rnd = self.rnd();
        for attempt in 0..2 {
            let Ok(current) = field.input_value().await else {
                return Ok(false);
            };
            if current == expected {
                if attempt > 0 {
                    self.count_variation("fixed_in_place");
                }
                return Ok(true);
            }
            let plan = correction_plan(&current, expected);
            // Noticing the mistake.
            self.sleep(rnd.uniform(300.0, 900.0)).await;
            if !self.put_caret(field, plan.caret).await? {
                return Ok(false);
            }
            for _ in 0..plan.delete {
                self.sleep(rnd.uniform(70.0, 180.0)).await;
                self.press(&page, "Backspace").await?;
            }
            if !plan.insert.is_empty() {
                self.type_text(field, plan.insert)
                    .focused(true)
                    .errors(false)
                    .await?;
            }
        }
        let ok = field.input_value().await.is_ok_and(|v| v == expected);
        self.count_variation(if ok { "fixed_in_place" } else { "fix_failed" });
        Ok(ok)
    }

    /// Add `text` to a field that already holds something: click right after
    /// it and type the separator and the text. Returns the expected final
    /// value, or `None` when the caret could not be placed.
    pub async fn append_to_field(
        &self,
        field: &Locator,
        text: &str,
        separator: &str,
    ) -> Result<Option<String>> {
        let page = page_of(field)?;
        let current = field.input_value().await.unwrap_or_default();
        let base = current.trim_end().to_string();
        let sep = if base.is_empty() { "" } else { separator };
        let expected = format!("{base}{sep}{text}");
        if !self.put_caret(field, current.chars().count()).await? {
            return Ok(None);
        }
        // Trailing spaces go first.
        for _ in base.chars().count()..current.chars().count() {
            self.press(&page, "Backspace").await?;
        }
        self.type_text(field, format!("{sep}{text}"))
            .focused(true)
            .errors(false)
            .await?;
        Ok(Some(expected))
    }

    /// For fields that must be right (an email, a password): type without
    /// typos, read the value back, and if it is wrong, fix it in place (a
    /// visible field) or clear and retype it carefully once (a password).
    /// Fails if it is still wrong.
    pub async fn type_verified(&self, field: &Locator, text: &str) -> Result<()> {
        // A prefilled field would join the old text with the new one.
        if field.input_value().await.is_ok_and(|v| !v.is_empty()) {
            let _ = field.clear().await;
        }
        self.type_text(field, text).errors(false).await?;
        let read = field.input_value().await?;
        if read == text {
            return Ok(());
        }
        let password = field
            .evaluate::<bool>("el => el.type === 'password'")
            .await
            .unwrap_or(true);
        if !password && self.fix_field(field, text).await? {
            return Ok(());
        }
        field.clear().await?;
        self.pause(super::PauseKind::Micro).await;
        self.type_text(field, text)
            .speed(Speed::Careful)
            .errors(false)
            .await?;
        if field.input_value().await? != text {
            return Err(Error::invalid_argument(
                "the field does not hold the typed text after two tries",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(current: &str, plan: &CorrectionPlan) -> String {
        let chars: Vec<char> = current.chars().collect();
        let mut out: String = chars[..plan.caret - plan.delete].iter().collect();
        out.push_str(&plan.insert);
        out.extend(&chars[plan.caret..]);
        out
    }

    #[test]
    fn plans_the_minimal_change() {
        let p = correction_plan("Lopez", "López");
        assert_eq!(
            p,
            CorrectionPlan {
                caret: 2,
                delete: 1,
                insert: "ó".into()
            }
        );
        let p = correction_plan("Garcia ", "García");
        assert_eq!((p.caret, p.delete, p.insert.as_str()), (7, 3, "ía"));
        for (a, b) in [
            ("abc", "abc"),
            ("", "abc"),
            ("abc", ""),
            ("ana@exmple.com", "ana@example.com"),
            ("aaa", "aa"),
            ("çã", "ção"),
        ] {
            assert_eq!(apply(a, &correction_plan(a, b)), b, "{a} -> {b}");
        }
    }
}
