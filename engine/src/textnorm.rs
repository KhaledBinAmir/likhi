//! Bengali and roman text normalization: a port of `src/likhi/engine/textnorm.py`.
//!
//! Every candidate the engine produces is keyed through these functions, so a single divergent
//! character here changes suggestions everywhere and does it silently. They are checked against
//! golden vectors dumped from the Python original (`tests/goldens/textnorm.jsonl`), which is the
//! only claim of correctness worth making about them.
//!
//! The rules, and why each exists (see the Python docstring for the research references):
//!
//! * ড় (U+09DC), ঢ় (U+09DD) and য় (U+09DF) are Unicode composition exclusions, so NFC
//!   *decomposes* them into letter + nukta. Keyboards emit the precomposed forms and corpora such
//!   as Dakshina are NFC, so comparing unnormalized strings gives false mismatches.
//! * Candrabindu (U+0981) belongs after the vowel sign, but people type it before. NFC will not
//!   reorder it, because vowel signs have combining class 0.
//! * ZWJ/ZWNJ survive NFC. They matter for rendering (র‍্য against র্য) but must not affect
//!   lexicon matching.
//! * The obsolete khanda-ta sequence (ত + virama + ZWJ) should be U+09CE.

use unicode_normalization::UnicodeNormalization;

pub const ZWNJ: char = '\u{200C}';
pub const ZWJ: char = '\u{200D}';
pub const VIRAMA: char = '\u{09CD}';
pub const NUKTA: char = '\u{09BC}';
pub const CANDRABINDU: char = '\u{0981}';
pub const KHANDA_TA: char = '\u{09CE}';

/// Dependent vowel signs, including the two-part ones and the length mark NFC decomposition uses.
/// This is the character class `"া-ৄেৈোৌৗৢৣ"` from the Python, expanded.
fn is_vowel_sign(c: char) -> bool {
    matches!(c,
        '\u{09BE}'..='\u{09C4}'
        | '\u{09C7}' | '\u{09C8}'
        | '\u{09CB}' | '\u{09CC}'
        | '\u{09D7}'
        | '\u{09E2}' | '\u{09E3}')
}

/// Internal canonical form: NFC, then the candrabindu reorder and the modern khanda-ta.
///
/// Idempotent. Joiners are kept, because they carry rendering intent.
pub fn canonical(text: &str) -> String {
    let nfc: String = text.nfc().collect();

    // Two rewrites in one pass over the NFC form. The Python applies them as two regex
    // substitutions in this order (khanda-ta first), and order matters: the khanda-ta rule consumes
    // a virama+ZWJ that the candrabindu rule would otherwise scan past harmlessly, but doing it the
    // other way round would still be a behaviour difference waiting to be discovered.
    let chars: Vec<char> = nfc.chars().collect();
    let mut stage1 = String::with_capacity(nfc.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{09A4}'
            && chars.get(i + 1) == Some(&VIRAMA)
            && chars.get(i + 2) == Some(&ZWJ)
        {
            stage1.push(KHANDA_TA);
            i += 3;
        } else {
            stage1.push(chars[i]);
            i += 1;
        }
    }

    // `CANDRABINDU([vowel signs]+)` -> `\1CANDRABINDU`: the sign or signs move in front of it.
    let chars: Vec<char> = stage1.chars().collect();
    let mut out = String::with_capacity(stage1.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == CANDRABINDU {
            let mut j = i + 1;
            while j < chars.len() && is_vowel_sign(chars[j]) {
                j += 1;
            }
            if j > i + 1 {
                out.extend(&chars[i + 1..j]);
                out.push(CANDRABINDU);
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Form used for equality and lexicon lookup: canonical, joiners removed, Assamese folded.
///
/// ৰ and ৱ are Assamese letters that look like Bengali র and ব. Folding them makes Assamese-typed
/// text match the Bengali lexicon; it is deliberately never done on output.
pub fn match_key(text: &str) -> String {
    canonical(text)
        .chars()
        .filter(|c| *c != ZWNJ && *c != ZWJ)
        .map(|c| match c {
            '\u{09F0}' => '\u{09B0}', // ৰ -> র
            '\u{09F1}' => '\u{09AC}', // ৱ -> ব
            other => other,
        })
        .collect()
}

/// Convert internal form to what gets typed into the application: canonical, with the nukta
/// letters recomposed, which is what most existing Bangla text and spell-checkers expect.
pub fn to_output(text: &str) -> String {
    let c = canonical(text);
    let chars: Vec<char> = c.chars().collect();
    let mut out = String::with_capacity(c.len());
    let mut i = 0;
    while i < chars.len() {
        if chars.get(i + 1) == Some(&NUKTA) {
            let recomposed = match chars[i] {
                '\u{09A1}' => Some('\u{09DC}'), // ড + nukta -> ড়
                '\u{09A2}' => Some('\u{09DD}'), // ঢ + nukta -> ঢ়
                '\u{09AF}' => Some('\u{09DF}'), // য + nukta -> য়
                _ => None,
            };
            if let Some(r) = recomposed {
                out.push(r);
                i += 2;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

pub const BANGLA_DIGITS: [char; 10] = ['০', '১', '২', '৩', '৪', '৫', '৬', '৭', '৮', '৯'];

pub fn to_bangla_digits(text: &str) -> String {
    text.chars()
        .map(|c| match c.to_digit(10) {
            Some(d) if c.is_ascii_digit() => BANGLA_DIGITS[d as usize],
            _ => c,
        })
        .collect()
}

pub fn to_western_digits(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{09E6}'..='\u{09EF}' => {
                char::from(b'0' + (c as u32 - 0x09E6) as u8)
            }
            other => other,
        })
        .collect()
}

pub fn has_bengali(text: &str) -> bool {
    text.chars().any(|c| ('\u{0980}'..='\u{09FF}').contains(&c))
}

/// Loose-romanization key: case-folded, ASCII letters/digits/apostrophe only.
///
/// Case is deliberately dropped: unlike Avro, Likhi never relies on capitalisation.
///
/// Python uses `str.casefold()`, which is stronger than lowercasing. The difference only matters
/// for characters that fold *into* ASCII, and after filtering to `[a-z0-9']` the only realistic one
/// is eszett: Python turns both ß and ẞ into "ss", which survives the filter, where Rust's
/// `to_lowercase` leaves ß alone and the filter then drops it. That single case is handled
/// explicitly below. Everything else that casefold and lowercase disagree on (Greek sigma, Cherokee,
/// the Turkish dotted I) either stays non-ASCII or agrees, and is filtered out either way.
pub fn normalize_roman(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'ß' | 'ẞ' => out.push_str("ss"),
            _ => {
                for lc in c.to_lowercase() {
                    if lc.is_ascii_lowercase() || lc.is_ascii_digit() || lc == '\'' {
                        out.push(lc);
                    }
                }
            }
        }
    }
    out
}

/// True when two Bengali strings are the same word modulo normalization noise.
pub fn equivalent(a: &str, b: &str) -> bool {
    match_key(a) == match_key(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nukta_round_trips() {
        // NFC decomposes the precomposed form, so canonical() of either spelling is the same, and
        // to_output() puts it back. This is the property the whole lexicon match depends on.
        let precomposed = "\u{09DC}";
        let decomposed = "\u{09A1}\u{09BC}";
        assert_eq!(canonical(precomposed), canonical(decomposed));
        assert_eq!(to_output(precomposed), "\u{09DC}");
        assert_eq!(to_output(decomposed), "\u{09DC}");
    }

    #[test]
    fn candrabindu_moves_after_the_vowel_sign() {
        let typed = "\u{0995}\u{0981}\u{09BE}"; // ক + candrabindu + aa-sign
        let want = "\u{0995}\u{09BE}\u{0981}"; // ক + aa-sign + candrabindu
        assert_eq!(canonical(typed), want);
        assert_eq!(canonical(&canonical(typed)), want, "must be idempotent");
    }

    #[test]
    fn legacy_khanda_ta_becomes_the_modern_codepoint() {
        assert_eq!(canonical("\u{09A4}\u{09CD}\u{200D}"), "\u{09CE}");
    }

    #[test]
    fn roman_keeps_only_what_the_lexicon_is_keyed_on() {
        assert_eq!(normalize_roman("AmAr"), "amar");
        assert_eq!(normalize_roman("ma'am"), "ma'am");
        assert_eq!(normalize_roman("hello-world"), "helloworld");
        assert_eq!(normalize_roman("café"), "caf");
        assert_eq!(normalize_roman(""), "");
    }

    #[test]
    fn joiners_are_kept_by_canonical_and_dropped_by_match_key() {
        let with_zwj = "\u{09B0}\u{200D}\u{09CD}\u{09AF}";
        assert!(canonical(with_zwj).contains(ZWJ));
        assert!(!match_key(with_zwj).contains(ZWJ));
    }
}
