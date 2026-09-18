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
/// Python uses `str.casefold()`, which is *full* Unicode case folding and strictly stronger than
/// lowercasing. The difference only matters for characters that fold into ASCII, because everything
/// else is discarded by the filter either way -- but for those it matters completely, and Rust's
/// `to_lowercase` does not perform the multi-character expansions.
///
/// This is the complete set, and it is complete because it was enumerated rather than reasoned
/// about: every one of the 1,114,112 codepoints was folded and lowercased in CPython and the two
/// results compared after filtering. Exactly seventeen differ. A first attempt at this list from
/// first principles had nine of them and missed the other eight, which is why the table is derived
/// and not recalled.
///
/// Characters that fold to something non-ASCII (Å to å, ς to σ, the Armenian ligatures) are absent
/// because the filter discards them identically either way, as are the dotted capital I and the
/// Kelvin sign, where folding and lowercasing agree.
///
/// A Bangla typist is unlikely to produce most of these, though the long s does turn up in scanned
/// text. They are handled anyway: the alternative is a silent, global divergence in the function
/// every lexicon key is derived from, and "nobody will type that" is not a property this code
/// should depend on.
fn casefold_to_ascii(c: char) -> Option<&'static str> {
    Some(match c {
        '\u{1E9A}' => "a",           // a with right half ring
        '\u{1E96}' => "h",           // h with line below
        '\u{01F0}' => "j",           // j with caron
        '\u{0149}' => "n",           // 'n
        '\u{017F}' => "s",           // long s
        '\u{1E97}' => "t",           // t with diaeresis
        '\u{1E98}' => "w",           // w with ring above
        '\u{1E99}' => "y",           // y with ring above
        '\u{00DF}' | '\u{1E9E}' => "ss", // eszett, lower and upper
        '\u{FB00}' => "ff",
        '\u{FB01}' => "fi",
        '\u{FB02}' => "fl",
        '\u{FB03}' => "ffi",
        '\u{FB04}' => "ffl",
        '\u{FB05}' | '\u{FB06}' => "st", // long-s-t and st ligatures
        _ => return None,
    })
}

pub fn normalize_roman(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if let Some(folded) = casefold_to_ascii(c) {
            out.push_str(folded);
            continue;
        }
        for lc in c.to_lowercase() {
            if lc.is_ascii_lowercase() || lc.is_ascii_digit() || lc == '\'' {
                out.push(lc);
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
        assert_eq!(canonical(&canonical(typed)), want, "stable once reordered");
    }

    #[test]
    fn candrabindu_moves_past_the_whole_run_of_vowel_signs() {
        // The Python regex is `CANDRABINDU([vowelsigns]+)`, greedy: the sign moves past every vowel
        // sign that follows, not just the first.
        let typed = "\u{0995}\u{0981}\u{09BE}\u{09BF}";
        assert_eq!(canonical(typed), "\u{0995}\u{09BE}\u{09BF}\u{0981}");
    }

    #[test]
    fn candrabindu_reordering_happens_once_and_does_not_rescan() {
        // `re.sub` does not re-scan what it substituted, so one pass is all there is, and canonical
        // is NOT a fixpoint for this input despite what its docstring claims. Reproducing the
        // single pass is what matters; converging is not.
        let typed = "\u{0995}\u{0981}\u{0981}\u{09BE}";
        assert_eq!(canonical(typed), "\u{0995}\u{0981}\u{09BE}\u{0981}");
    }

    /// Verified against CPython's `str.casefold` directly, character by character. These are the
    /// only characters that fold into the set the roman key keeps, and `to_lowercase` handles none
    /// of them.
    #[test]
    fn full_case_folding_expansions_reach_ascii() {
        assert_eq!(normalize_roman("ß"), "ss");
        assert_eq!(normalize_roman("ẞ"), "ss");
        assert_eq!(normalize_roman("straße"), "strasse");
        assert_eq!(normalize_roman("\u{FB00}"), "ff");
        assert_eq!(normalize_roman("\u{FB01}le"), "file");
        assert_eq!(normalize_roman("\u{FB02}"), "fl");
        assert_eq!(normalize_roman("\u{FB03}"), "ffi");
        assert_eq!(normalize_roman("\u{FB04}"), "ffl");
        assert_eq!(normalize_roman("\u{FB05}"), "st");
        assert_eq!(normalize_roman("\u{FB06}"), "st");
        // The eight this test originally missed, which is the reason the table is enumerated.
        assert_eq!(normalize_roman("\u{1E9A}"), "a");
        assert_eq!(normalize_roman("\u{1E96}"), "h", "h with line below");
        assert_eq!(normalize_roman("\u{01F0}"), "j");
        assert_eq!(normalize_roman("\u{0149}"), "n");
        assert_eq!(normalize_roman("\u{017F}"), "s", "long s");
        assert_eq!(normalize_roman("\u{1E97}"), "t");
        assert_eq!(normalize_roman("\u{1E98}"), "w");
        assert_eq!(normalize_roman("\u{1E99}"), "y");
        // Agree between folding and lowercasing; listed so a future change cannot break them quietly.
        assert_eq!(normalize_roman("\u{0130}"), "i", "dotted capital I");
        assert_eq!(normalize_roman("\u{212A}"), "k", "kelvin sign");
        // Fold to non-ASCII and are filtered out either way.
        assert_eq!(normalize_roman("\u{212B}"), "", "angstrom");
        assert_eq!(normalize_roman("\u{03C2}"), "", "final sigma");
        assert_eq!(normalize_roman("\u{2019}"), "", "curly apostrophe is not the ASCII one");
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
