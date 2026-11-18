//! Keyboard layouts: which physical keys a person presses to type each
//! character (Shift, AltGr, dead keys), and sending those key events.
//!
//! Playwright's `keyboard.down` only knows the US layout and never presses
//! Shift, so `@` or `A` arrive with `shiftKey = false` and `ç` is not a key at
//! all. Here each character is typed with the keys of the chosen layout.
//!
//! Known limits: `keyCode` follows the US virtual key codes, and
//! `getModifierState('AltGraph')` stays false (the protocol has no such
//! modifier).

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{Value, json};
use unicode_normalization_lite::nfd;

/// A keyboard layout (Windows).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeyboardLayout {
    /// United States.
    #[default]
    Us,
    /// United Kingdom.
    Uk,
    /// Portugal.
    Pt,
    /// Spain.
    Es,
}

impl KeyboardLayout {
    /// The layout of a browser language (`navigator.language`): by its
    /// region when it has one (`es-ES` → Spain, `en-GB` → UK, `pt-BR` → US,
    /// which has no layout here), else by the language (`es` → Spain, `pt`
    /// → Portugal); US for any other.
    pub fn for_locale(locale: &str) -> Self {
        let mut parts = locale.trim().split(['-', '_']);
        let language = parts.next().unwrap_or("").to_ascii_lowercase();
        // The region is the 2-letter part (`zh-Hant-TW`: TW).
        if let Some(region) =
            parts.find(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))
        {
            return Self::for_country(region);
        }
        match language.as_str() {
            "es" | "ca" | "gl" | "eu" => Self::Es,
            "pt" => Self::Pt,
            _ => Self::Us,
        }
    }

    /// The layout people use in a country (ISO code); US for any other.
    pub fn for_country(country: &str) -> Self {
        match country.trim().to_ascii_uppercase().as_str() {
            "PT" => Self::Pt,
            "ES" => Self::Es,
            "UK" | "GB" => Self::Uk,
            _ => Self::Us,
        }
    }
}

/// One physical key press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stroke {
    pub code: String,
    pub key: String,
    /// The inserted text; empty on a dead key.
    pub text: String,
    pub shift: bool,
    pub altgr: bool,
}

type Table = &'static [(&'static str, &'static [&'static str])];

const US: Table = &[
    ("Backquote", &["`", "~"]),
    ("Digit1", &["1", "!"]),
    ("Digit2", &["2", "@"]),
    ("Digit3", &["3", "#"]),
    ("Digit4", &["4", "$"]),
    ("Digit5", &["5", "%"]),
    ("Digit6", &["6", "^"]),
    ("Digit7", &["7", "&"]),
    ("Digit8", &["8", "*"]),
    ("Digit9", &["9", "("]),
    ("Digit0", &["0", ")"]),
    ("Minus", &["-", "_"]),
    ("Equal", &["=", "+"]),
    ("BracketLeft", &["[", "{"]),
    ("BracketRight", &["]", "}"]),
    ("Backslash", &["\\", "|"]),
    ("Semicolon", &[";", ":"]),
    ("Quote", &["'", "\""]),
    ("Comma", &[",", "<"]),
    ("Period", &[".", ">"]),
    ("Slash", &["/", "?"]),
];

const UK: Table = &[
    ("Backquote", &["`", "¬", "¦"]),
    ("Digit1", &["1", "!"]),
    ("Digit2", &["2", "\""]),
    ("Digit3", &["3", "£"]),
    ("Digit4", &["4", "$", "€"]),
    ("Digit5", &["5", "%"]),
    ("Digit6", &["6", "^"]),
    ("Digit7", &["7", "&"]),
    ("Digit8", &["8", "*"]),
    ("Digit9", &["9", "("]),
    ("Digit0", &["0", ")"]),
    ("Minus", &["-", "_"]),
    ("Equal", &["=", "+"]),
    ("BracketLeft", &["[", "{"]),
    ("BracketRight", &["]", "}"]),
    ("Semicolon", &[";", ":"]),
    ("Quote", &["'", "@"]),
    ("Backslash", &["#", "~"]),
    ("IntlBackslash", &["\\", "|"]),
    ("Comma", &[",", "<"]),
    ("Period", &[".", ">"]),
    ("Slash", &["/", "?"]),
    // AltGr + vowel: acute accent.
    ("KeyA", &["", "", "á", "Á"]),
    ("KeyE", &["", "", "é", "É"]),
    ("KeyI", &["", "", "í", "Í"]),
    ("KeyO", &["", "", "ó", "Ó"]),
    ("KeyU", &["", "", "ú", "Ú"]),
];

const PT: Table = &[
    ("Backquote", &["\\", "|"]),
    ("Digit1", &["1", "!"]),
    ("Digit2", &["2", "\"", "@"]),
    ("Digit3", &["3", "#", "£"]),
    ("Digit4", &["4", "$", "§"]),
    ("Digit5", &["5", "%"]),
    ("Digit6", &["6", "&"]),
    ("Digit7", &["7", "/", "{"]),
    ("Digit8", &["8", "(", "["]),
    ("Digit9", &["9", ")", "]"]),
    ("Digit0", &["0", "=", "}"]),
    ("Minus", &["'", "?"]),
    ("Equal", &["«", "»"]),
    ("BracketLeft", &["+", "*"]),
    ("Semicolon", &["ç", "Ç"]),
    ("Quote", &["º", "ª"]),
    ("IntlBackslash", &["<", ">"]),
    ("Comma", &[",", ";"]),
    ("Period", &[".", ":"]),
    ("Slash", &["-", "_"]),
    ("KeyE", &["", "", "€"]),
];

const ES: Table = &[
    ("Backquote", &["º", "ª", "\\"]),
    ("Digit1", &["1", "!", "|"]),
    ("Digit2", &["2", "\"", "@"]),
    ("Digit3", &["3", "·", "#"]),
    ("Digit4", &["4", "$", "~"]),
    ("Digit5", &["5", "%"]),
    ("Digit6", &["6", "&", "¬"]),
    ("Digit7", &["7", "/"]),
    ("Digit8", &["8", "("]),
    ("Digit9", &["9", ")"]),
    ("Digit0", &["0", "="]),
    ("Minus", &["'", "?"]),
    ("Equal", &["¡", "¿"]),
    ("BracketLeft", &["", "", "["]),
    ("BracketRight", &["+", "*", "]"]),
    ("Semicolon", &["ñ", "Ñ"]),
    ("Quote", &["", "", "{"]),
    ("Backslash", &["ç", "Ç", "}"]),
    ("IntlBackslash", &["<", ">"]),
    ("Comma", &[",", ";"]),
    ("Period", &[".", ":"]),
    ("Slash", &["-", "_"]),
    ("KeyE", &["", "", "€"]),
];

/// Dead keys: combining mark → (code, level). Levels: 0 plain, 1 Shift,
/// 2 AltGr, 3 Shift+AltGr.
fn dead_keys(layout: KeyboardLayout) -> &'static [(char, &'static str, u8)] {
    match layout {
        KeyboardLayout::Us | KeyboardLayout::Uk => &[],
        KeyboardLayout::Pt => &[
            ('\u{301}', "BracketRight", 0),
            ('\u{300}', "BracketRight", 1),
            ('\u{303}', "Backslash", 0),
            ('\u{302}', "Backslash", 1),
            ('\u{308}', "BracketLeft", 2),
        ],
        KeyboardLayout::Es => &[
            ('\u{301}', "Quote", 0),
            ('\u{308}', "Quote", 1),
            ('\u{300}', "BracketLeft", 0),
            ('\u{302}', "BracketLeft", 1),
        ],
    }
}

fn direct(layout: KeyboardLayout) -> &'static HashMap<char, Stroke> {
    static MAPS: OnceLock<HashMap<KeyboardLayout, HashMap<char, Stroke>>> = OnceLock::new();
    let maps = MAPS.get_or_init(|| {
        let mut out = HashMap::new();
        for layout in [
            KeyboardLayout::Us,
            KeyboardLayout::Uk,
            KeyboardLayout::Pt,
            KeyboardLayout::Es,
        ] {
            let mut m: HashMap<char, Stroke> = HashMap::new();
            let mut add = |code: &str, ch: &str, level: u8| {
                let Some(c) = ch.chars().next() else { return };
                m.entry(c).or_insert_with(|| Stroke {
                    code: code.to_string(),
                    key: ch.to_string(),
                    text: ch.to_string(),
                    shift: level == 1 || level == 3,
                    altgr: level >= 2,
                });
            };
            for c in b'a'..=b'z' {
                let l = (c as char).to_string();
                let code = format!("Key{}", l.to_uppercase());
                add(&code, &l, 0);
                add(&code, &l.to_uppercase(), 1);
            }
            add("Space", " ", 0);
            for n in 0..10 {
                add(&format!("Digit{n}"), &n.to_string(), 0);
            }
            let table = match layout {
                KeyboardLayout::Us => US,
                KeyboardLayout::Uk => UK,
                KeyboardLayout::Pt => PT,
                KeyboardLayout::Es => ES,
            };
            for (code, chars) in table {
                for (level, ch) in chars.iter().enumerate() {
                    add(code, ch, level as u8);
                }
            }
            out.insert(layout, m);
        }
        out
    });
    &maps[&layout]
}

/// The strokes to type `ch` with this layout: one (a direct key), two (dead
/// key + letter) or `None` when the layout cannot type it.
pub(crate) fn plan(ch: char, layout: KeyboardLayout) -> Option<Vec<Stroke>> {
    if let Some(stroke) = direct(layout).get(&ch) {
        return Some(vec![stroke.clone()]);
    }
    let decomposed = nfd(ch);
    let [base, mark] = decomposed.as_slice() else {
        return None;
    };
    let (_, code, level) = dead_keys(layout).iter().find(|(m, _, _)| m == mark)?;
    let letter = direct(layout).get(base)?;
    Some(vec![
        Stroke {
            code: code.to_string(),
            key: "Dead".into(),
            text: String::new(),
            shift: *level == 1 || *level == 3,
            altgr: *level >= 2,
        },
        Stroke {
            key: ch.to_string(),
            text: ch.to_string(),
            ..letter.clone()
        },
    ])
}

/// The Windows virtual key code (US based) of a key: the event's `keyCode`.
pub(crate) fn virtual_key_code(code: &str) -> i64 {
    if let Some(l) = code.strip_prefix("Key").filter(|l| l.len() == 1) {
        return l.as_bytes()[0] as i64;
    }
    if let Some(n) = code
        .strip_prefix("Digit")
        .and_then(|d| d.parse::<i64>().ok())
    {
        return 48 + n;
    }
    match code {
        "Space" => 32,
        "Backquote" => 192,
        "Minus" => 189,
        "Equal" => 187,
        "BracketLeft" => 219,
        "BracketRight" => 221,
        "Backslash" => 220,
        "Semicolon" => 186,
        "Quote" => 222,
        "Comma" => 188,
        "Period" => 190,
        "Slash" => 191,
        "IntlBackslash" => 226,
        "ShiftLeft" | "ShiftRight" => 16,
        "ControlLeft" => 17,
        "AltRight" => 18,
        _ => 0,
    }
}

// Protocol modifier bits. AltGr on Windows is Ctrl + Alt.
pub(crate) const MOD_ALT: i64 = 1;
pub(crate) const MOD_CTRL: i64 = 2;
pub(crate) const MOD_SHIFT: i64 = 8;

/// The `Input.dispatchKeyEvent` parameters of a key down or up. `text` only
/// on the key down of a key that types.
pub(crate) fn key_event(
    down: bool,
    key: &str,
    code: &str,
    text: &str,
    modifiers: i64,
    location: i64,
) -> Value {
    let kind = if !down {
        "keyUp"
    } else if text.is_empty() {
        "rawKeyDown"
    } else {
        "keyDown"
    };
    let mut params = json!({
        "type": kind,
        "key": key,
        "code": code,
        "windowsVirtualKeyCode": virtual_key_code(code),
        "modifiers": modifiers,
    });
    if location != 0 {
        params["location"] = json!(location);
    }
    if down && !text.is_empty() {
        params["text"] = json!(text);
        params["unmodifiedText"] = json!(text);
    }
    params
}

/// Canonical decomposition of the accented letters the layouts have dead keys for.
mod unicode_normalization_lite {
    /// `ch` as base letter + combining mark, or `[ch]` when it does not decompose.
    pub(crate) fn nfd(ch: char) -> Vec<char> {
        const TABLE: &[(char, char, char)] = &[
            ('á', 'a', '\u{301}'),
            ('é', 'e', '\u{301}'),
            ('í', 'i', '\u{301}'),
            ('ó', 'o', '\u{301}'),
            ('ú', 'u', '\u{301}'),
            ('ý', 'y', '\u{301}'),
            ('Á', 'A', '\u{301}'),
            ('É', 'E', '\u{301}'),
            ('Í', 'I', '\u{301}'),
            ('Ó', 'O', '\u{301}'),
            ('Ú', 'U', '\u{301}'),
            ('Ý', 'Y', '\u{301}'),
            ('à', 'a', '\u{300}'),
            ('è', 'e', '\u{300}'),
            ('ì', 'i', '\u{300}'),
            ('ò', 'o', '\u{300}'),
            ('ù', 'u', '\u{300}'),
            ('À', 'A', '\u{300}'),
            ('È', 'E', '\u{300}'),
            ('Ì', 'I', '\u{300}'),
            ('Ò', 'O', '\u{300}'),
            ('Ù', 'U', '\u{300}'),
            ('â', 'a', '\u{302}'),
            ('ê', 'e', '\u{302}'),
            ('î', 'i', '\u{302}'),
            ('ô', 'o', '\u{302}'),
            ('û', 'u', '\u{302}'),
            ('Â', 'A', '\u{302}'),
            ('Ê', 'E', '\u{302}'),
            ('Î', 'I', '\u{302}'),
            ('Ô', 'O', '\u{302}'),
            ('Û', 'U', '\u{302}'),
            ('ã', 'a', '\u{303}'),
            ('õ', 'o', '\u{303}'),
            ('ñ', 'n', '\u{303}'),
            ('Ã', 'A', '\u{303}'),
            ('Õ', 'O', '\u{303}'),
            ('Ñ', 'N', '\u{303}'),
            ('ä', 'a', '\u{308}'),
            ('ë', 'e', '\u{308}'),
            ('ï', 'i', '\u{308}'),
            ('ö', 'o', '\u{308}'),
            ('ü', 'u', '\u{308}'),
            ('ÿ', 'y', '\u{308}'),
            ('Ä', 'A', '\u{308}'),
            ('Ë', 'E', '\u{308}'),
            ('Ï', 'I', '\u{308}'),
            ('Ö', 'O', '\u{308}'),
            ('Ü', 'U', '\u{308}'),
        ];
        TABLE
            .iter()
            .find(|(c, _, _)| *c == ch)
            .map_or_else(|| vec![ch], |(_, base, mark)| vec![*base, *mark])
    }
}

/// The first letter of `ch` without its accent (for the hand that types it).
pub(crate) fn base_letter(ch: char) -> char {
    let lower = ch.to_lowercase().next().unwrap_or(ch);
    nfd(lower)[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(ch: char, layout: KeyboardLayout) -> Stroke {
        let mut strokes = plan(ch, layout).unwrap();
        assert_eq!(strokes.len(), 1, "{ch}");
        strokes.remove(0)
    }

    #[test]
    fn uppercase_and_symbols_press_shift_or_altgr_per_layout() {
        let a = one('A', KeyboardLayout::Us);
        assert_eq!((a.code.as_str(), a.shift, a.altgr), ("KeyA", true, false));
        let at_us = one('@', KeyboardLayout::Us);
        assert_eq!((at_us.code.as_str(), at_us.shift), ("Digit2", true));
        let at_pt = one('@', KeyboardLayout::Pt);
        assert_eq!(
            (at_pt.code.as_str(), at_pt.shift, at_pt.altgr),
            ("Digit2", false, true)
        );
        let at_uk = one('@', KeyboardLayout::Uk);
        assert_eq!((at_uk.code.as_str(), at_uk.shift), ("Quote", true));
        let c = one('ç', KeyboardLayout::Pt);
        assert_eq!(c.code, "Semicolon");
        let n = one('ñ', KeyboardLayout::Es);
        assert_eq!(n.code, "Semicolon");
    }

    #[test]
    fn accents_use_dead_keys() {
        let s = plan('á', KeyboardLayout::Pt).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(
            (s[0].key.as_str(), s[0].code.as_str(), s[0].text.as_str()),
            ("Dead", "BracketRight", "")
        );
        assert_eq!((s[1].code.as_str(), s[1].text.as_str()), ("KeyA", "á"));
        let s = plan('Ã', KeyboardLayout::Pt).unwrap();
        assert_eq!((s[0].code.as_str(), s[1].shift), ("Backslash", true));
        let s = plan('é', KeyboardLayout::Es).unwrap();
        assert_eq!(s[0].code, "Quote");
        // UK types it directly with AltGr.
        assert!(one('é', KeyboardLayout::Uk).altgr);
        // US cannot type it.
        assert!(plan('é', KeyboardLayout::Us).is_none());
    }

    #[test]
    fn layout_from_the_browser_language() {
        assert_eq!(KeyboardLayout::for_locale("es-ES"), KeyboardLayout::Es);
        assert_eq!(KeyboardLayout::for_locale("en-GB"), KeyboardLayout::Uk);
        assert_eq!(KeyboardLayout::for_locale("pt-PT"), KeyboardLayout::Pt);
        assert_eq!(KeyboardLayout::for_locale("pt_PT"), KeyboardLayout::Pt);
        assert_eq!(KeyboardLayout::for_locale("en-US"), KeyboardLayout::Us);
        assert_eq!(KeyboardLayout::for_locale("pt-BR"), KeyboardLayout::Us);
        assert_eq!(KeyboardLayout::for_locale("es"), KeyboardLayout::Es);
        assert_eq!(KeyboardLayout::for_locale("pt"), KeyboardLayout::Pt);
        assert_eq!(KeyboardLayout::for_locale("de-DE"), KeyboardLayout::Us);
        assert_eq!(KeyboardLayout::for_locale(""), KeyboardLayout::Us);
    }

    #[test]
    fn virtual_key_codes() {
        assert_eq!(virtual_key_code("KeyA"), 65);
        assert_eq!(virtual_key_code("Digit2"), 50);
        assert_eq!(virtual_key_code("Quote"), 222);
    }
}
