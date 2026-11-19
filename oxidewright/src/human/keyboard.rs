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
        // Every Latin letter whose canonical decomposition is an ASCII
        // letter plus one of the marks the dead keys type (grave, acute,
        // circumflex, tilde, diaeresis), or a cedilla (for `base_letter`).
        const TABLE: &[(char, char, char)] = &[
            ('À', 'A', '\u{300}'),
            ('Á', 'A', '\u{301}'),
            ('Â', 'A', '\u{302}'),
            ('Ã', 'A', '\u{303}'),
            ('Ä', 'A', '\u{308}'),
            ('Ç', 'C', '\u{327}'),
            ('È', 'E', '\u{300}'),
            ('É', 'E', '\u{301}'),
            ('Ê', 'E', '\u{302}'),
            ('Ë', 'E', '\u{308}'),
            ('Ì', 'I', '\u{300}'),
            ('Í', 'I', '\u{301}'),
            ('Î', 'I', '\u{302}'),
            ('Ï', 'I', '\u{308}'),
            ('Ñ', 'N', '\u{303}'),
            ('Ò', 'O', '\u{300}'),
            ('Ó', 'O', '\u{301}'),
            ('Ô', 'O', '\u{302}'),
            ('Õ', 'O', '\u{303}'),
            ('Ö', 'O', '\u{308}'),
            ('Ù', 'U', '\u{300}'),
            ('Ú', 'U', '\u{301}'),
            ('Û', 'U', '\u{302}'),
            ('Ü', 'U', '\u{308}'),
            ('Ý', 'Y', '\u{301}'),
            ('à', 'a', '\u{300}'),
            ('á', 'a', '\u{301}'),
            ('â', 'a', '\u{302}'),
            ('ã', 'a', '\u{303}'),
            ('ä', 'a', '\u{308}'),
            ('ç', 'c', '\u{327}'),
            ('è', 'e', '\u{300}'),
            ('é', 'e', '\u{301}'),
            ('ê', 'e', '\u{302}'),
            ('ë', 'e', '\u{308}'),
            ('ì', 'i', '\u{300}'),
            ('í', 'i', '\u{301}'),
            ('î', 'i', '\u{302}'),
            ('ï', 'i', '\u{308}'),
            ('ñ', 'n', '\u{303}'),
            ('ò', 'o', '\u{300}'),
            ('ó', 'o', '\u{301}'),
            ('ô', 'o', '\u{302}'),
            ('õ', 'o', '\u{303}'),
            ('ö', 'o', '\u{308}'),
            ('ù', 'u', '\u{300}'),
            ('ú', 'u', '\u{301}'),
            ('û', 'u', '\u{302}'),
            ('ü', 'u', '\u{308}'),
            ('ý', 'y', '\u{301}'),
            ('ÿ', 'y', '\u{308}'),
            ('Ć', 'C', '\u{301}'),
            ('ć', 'c', '\u{301}'),
            ('Ĉ', 'C', '\u{302}'),
            ('ĉ', 'c', '\u{302}'),
            ('Ĝ', 'G', '\u{302}'),
            ('ĝ', 'g', '\u{302}'),
            ('Ģ', 'G', '\u{327}'),
            ('ģ', 'g', '\u{327}'),
            ('Ĥ', 'H', '\u{302}'),
            ('ĥ', 'h', '\u{302}'),
            ('Ĩ', 'I', '\u{303}'),
            ('ĩ', 'i', '\u{303}'),
            ('Ĵ', 'J', '\u{302}'),
            ('ĵ', 'j', '\u{302}'),
            ('Ķ', 'K', '\u{327}'),
            ('ķ', 'k', '\u{327}'),
            ('Ĺ', 'L', '\u{301}'),
            ('ĺ', 'l', '\u{301}'),
            ('Ļ', 'L', '\u{327}'),
            ('ļ', 'l', '\u{327}'),
            ('Ń', 'N', '\u{301}'),
            ('ń', 'n', '\u{301}'),
            ('Ņ', 'N', '\u{327}'),
            ('ņ', 'n', '\u{327}'),
            ('Ŕ', 'R', '\u{301}'),
            ('ŕ', 'r', '\u{301}'),
            ('Ŗ', 'R', '\u{327}'),
            ('ŗ', 'r', '\u{327}'),
            ('Ś', 'S', '\u{301}'),
            ('ś', 's', '\u{301}'),
            ('Ŝ', 'S', '\u{302}'),
            ('ŝ', 's', '\u{302}'),
            ('Ş', 'S', '\u{327}'),
            ('ş', 's', '\u{327}'),
            ('Ţ', 'T', '\u{327}'),
            ('ţ', 't', '\u{327}'),
            ('Ũ', 'U', '\u{303}'),
            ('ũ', 'u', '\u{303}'),
            ('Ŵ', 'W', '\u{302}'),
            ('ŵ', 'w', '\u{302}'),
            ('Ŷ', 'Y', '\u{302}'),
            ('ŷ', 'y', '\u{302}'),
            ('Ÿ', 'Y', '\u{308}'),
            ('Ź', 'Z', '\u{301}'),
            ('ź', 'z', '\u{301}'),
            ('Ǵ', 'G', '\u{301}'),
            ('ǵ', 'g', '\u{301}'),
            ('Ǹ', 'N', '\u{300}'),
            ('ǹ', 'n', '\u{300}'),
            ('Ȩ', 'E', '\u{327}'),
            ('ȩ', 'e', '\u{327}'),
            ('Ḑ', 'D', '\u{327}'),
            ('ḑ', 'd', '\u{327}'),
            ('Ḧ', 'H', '\u{308}'),
            ('ḧ', 'h', '\u{308}'),
            ('Ḩ', 'H', '\u{327}'),
            ('ḩ', 'h', '\u{327}'),
            ('Ḱ', 'K', '\u{301}'),
            ('ḱ', 'k', '\u{301}'),
            ('Ḿ', 'M', '\u{301}'),
            ('ḿ', 'm', '\u{301}'),
            ('Ṕ', 'P', '\u{301}'),
            ('ṕ', 'p', '\u{301}'),
            ('Ṽ', 'V', '\u{303}'),
            ('ṽ', 'v', '\u{303}'),
            ('Ẁ', 'W', '\u{300}'),
            ('ẁ', 'w', '\u{300}'),
            ('Ẃ', 'W', '\u{301}'),
            ('ẃ', 'w', '\u{301}'),
            ('Ẅ', 'W', '\u{308}'),
            ('ẅ', 'w', '\u{308}'),
            ('Ẍ', 'X', '\u{308}'),
            ('ẍ', 'x', '\u{308}'),
            ('Ẑ', 'Z', '\u{302}'),
            ('ẑ', 'z', '\u{302}'),
            ('ẗ', 't', '\u{308}'),
            ('Ẽ', 'E', '\u{303}'),
            ('ẽ', 'e', '\u{303}'),
            ('Ỳ', 'Y', '\u{300}'),
            ('ỳ', 'y', '\u{300}'),
            ('Ỹ', 'Y', '\u{303}'),
            ('ỹ', 'y', '\u{303}'),
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
        // Any letter takes the dead key, not only the vowels.
        let s = plan('ẽ', KeyboardLayout::Pt).unwrap();
        assert_eq!(
            (s[0].code.as_str(), s[1].code.as_str(), s[1].text.as_str()),
            ("Backslash", "KeyE", "ẽ")
        );
        let s = plan('ś', KeyboardLayout::Es).unwrap();
        assert_eq!((s[0].code.as_str(), s[1].code.as_str()), ("Quote", "KeyS"));
        let s = plan('Ǹ', KeyboardLayout::Pt).unwrap();
        assert_eq!(
            (s[0].code.as_str(), s[0].shift, s[1].shift),
            ("BracketRight", true, true)
        );
        // A cedilla has no dead key.
        assert!(plan('ş', KeyboardLayout::Pt).is_none());
        assert!(plan('ç', KeyboardLayout::Us).is_none());
    }

    #[test]
    fn base_letters_drop_the_accent() {
        assert_eq!(base_letter('Ç'), 'c');
        assert_eq!(base_letter('Ã'), 'a');
        assert_eq!(base_letter('ẽ'), 'e');
        assert_eq!(base_letter('@'), '@');
        // Every entry decomposes into an ASCII letter of the same case.
        for c in ('\u{c0}'..='\u{24f}').chain('\u{1e00}'..='\u{1eff}') {
            let d = nfd(c);
            if d.len() == 2 {
                assert!(d[0].is_ascii_alphabetic(), "{c}");
                assert_eq!(d[0].is_uppercase(), c.is_uppercase(), "{c}");
            }
        }
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
