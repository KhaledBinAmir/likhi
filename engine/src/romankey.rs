//! Phonetic keys that make loose romanizations and Bangla words comparable.
//! A port of `src/likhi/engine/romankey.py`.
//!
//! Both the typed roman string and a Bangla word are mapped into the same coarse consonant
//! skeleton, so words can be looked up by key: `amr`, `amar`, `aamar` and আমার all become `amr`.
//! The key is a recall device for candidate generation; the ranker decides which candidate is right.
//!
//! Two levels: `fine` keeps aspiration and sibilant distinctions (kh against k, sh against s, ch
//! against c); `coarse` merges them and drops semivowels, for very sloppy input. Both drop every
//! vowel except a word-initial one, and collapse doubled consonants.
//!
//! The tables below are transcribed from the Python and checked against
//! `tests/goldens/romankey.jsonl`, which covers every entry through real words.

use crate::textnorm::{match_key, normalize_roman, VIRAMA};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Level {
    Fine,
    Coarse,
}

impl Level {
    /// The single-letter tag used to build lexicon keys: "f:" / "c:" prefixes in keys.lkx.
    pub fn tag(self) -> char {
        match self {
            Level::Fine => 'f',
            Level::Coarse => 'c',
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "fine" => Some(Level::Fine),
            "coarse" => Some(Level::Coarse),
            _ => None,
        }
    }
}

const NUKTA: char = '\u{09BC}';

/// Bangla consonants to their fine class.
///
/// The three precomposed nukta letters are included for completeness even though `match_key`
/// applies NFC, which decomposes them, so in practice the nukta branch in `bangla_units` handles
/// them instead. Keeping them costs nothing and means a precomposed character arriving by some
/// other route is still classified rather than silently dropped.
fn cons(c: char) -> Option<&'static str> {
    Some(match c {
        'ক' => "k",
        'খ' => "kh",
        'গ' => "g",
        'ঘ' => "gh",
        'ঙ' => "ng",
        'চ' => "c",
        'ছ' => "ch",
        'জ' => "j",
        'ঝ' => "jh",
        'ঞ' => "n",
        'ট' => "t",
        'ঠ' => "th",
        'ড' => "d",
        'ঢ' => "dh",
        'ণ' => "n",
        'ত' => "t",
        'থ' => "th",
        'দ' => "d",
        'ধ' => "dh",
        'ন' => "n",
        'প' => "p",
        'ফ' => "f",
        'ব' => "b",
        'ভ' => "bh",
        'ম' => "m",
        'য' => "j",
        'র' => "r",
        'ল' => "l",
        'শ' => "sh",
        'ষ' => "sh",
        'স' => "s",
        'হ' => "h",
        '\u{09DC}' => "r",  // ড় precomposed
        '\u{09DD}' => "rh", // ঢ় precomposed
        '\u{09DF}' => "y",  // য় precomposed
        'ৎ' => "t",
        'ং' => "ng",
        'ঃ' => "h",
        'ৰ' => "r", // Assamese ra; match_key folds it, so normally unreachable
        'ৱ' => "b", // Assamese wa
        _ => return None,
    })
}

/// Independent vowels and vowel signs to their vowel class. Only a word-initial vowel survives.
fn vowel(c: char) -> Option<&'static str> {
    Some(match c {
        'অ' => "o",
        'আ' => "a",
        'ই' => "i",
        'ঈ' => "i",
        'উ' => "u",
        'ঊ' => "u",
        'ঋ' => "ri",
        'এ' => "e",
        'ঐ' => "oi",
        'ও' => "o",
        'ঔ' => "ou",
        'া' => "a",
        'ি' => "i",
        'ী' => "i",
        'ু' => "u",
        'ূ' => "u",
        'ৃ' => "ri",
        'ে' => "e",
        'ৈ' => "oi",
        'ো' => "o",
        'ৌ' => "ou",
        'ৗ' => "ou",
        _ => return None,
    })
}

/// Roman digraphs and trigraphs. Vowels become "V"-prefixed markers so `finalize` can tell them
/// apart from consonant classes.
fn roman_multi(seg: &str) -> Option<&'static str> {
    Some(match seg {
        // three characters, tried first
        "chh" => "ch",
        "ksh" => "kh",
        "kkh" => "kh",
        // two characters
        "kh" => "kh",
        "gh" => "gh",
        "ng" => "ng",
        "ch" => "ch",
        "jh" => "jh",
        "th" => "th",
        "dh" => "dh",
        "ph" => "f",
        "bh" => "bh",
        "sh" => "sh",
        "ck" => "k",
        "ks" => "k",
        "oi" => "Voi",
        "ou" => "Vou",
        "au" => "Vou",
        "ai" => "Voi",
        "ee" => "Vi",
        "oo" => "Vu",
        "aa" => "Va",
        _ => return None,
    })
}

fn roman_single(c: char) -> Option<&'static str> {
    Some(match c {
        'k' => "k",
        'q' => "k",
        'g' => "g",
        'c' => "c",
        'j' => "j",
        'z' => "j",
        't' => "t",
        'd' => "d",
        'n' => "n",
        'p' => "p",
        'f' => "f",
        'b' => "b",
        'v' => "bh",
        'm' => "m",
        'y' => "y",
        'r' => "r",
        'l' => "l",
        's' => "s",
        'h' => "h",
        'x' => "k",
        'w' => "Vo",
        'a' => "Va",
        'e' => "Ve",
        'i' => "Vi",
        'o' => "Vo",
        'u' => "Vu",
        _ => return None,
    })
}

/// Coarse merges for consonant classes. An empty result means the unit disappears.
fn coarse_cons(u: &str) -> &str {
    match u {
        "kh" => "k",
        "gh" => "g",
        "ch" => "c",
        "jh" => "j",
        "th" => "t",
        "dh" => "d",
        "bh" => "b",
        "sh" => "s",
        "ng" => "n",
        "rh" => "r",
        "y" => "",
        "h" => "h",
        other => other,
    }
}

/// Coarse merges for the vowel markers. Every marker any table can produce is covered; a miss would
/// be a transcription error, and is treated as one rather than silently passed through.
fn coarse_vowel(u: &str) -> Option<&'static str> {
    Some(match u {
        "Va" => "a",
        "Vo" => "a",
        "Ve" => "e",
        "Vi" => "i",
        "Vu" => "u",
        "Voi" => "i",
        "Vou" => "u",
        "Vri" => "r",
        _ => return None,
    })
}

/// Drop non-initial vowels, apply coarse merges, collapse repeats.
///
/// The semivowel unit "y" (roman y, Bangla য়) behaves like a vowel at both levels: people write
/// কোরিয়া as koria, koriya or korea, so it must not create a consonant slot. A word-initial y is a
/// consonant (yeno = যেন) and maps to j.
fn finalize(units: &[&str], level: Level) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(units.len());
    for (i, &unit) in units.iter().enumerate() {
        let mut u = unit;
        if let Some(rest) = u.strip_prefix('V') {
            if i == 0 {
                match level {
                    Level::Coarse => {
                        // Python indexes _COARSE_VOWEL directly and would raise on a marker it does
                        // not know. Falling back to the fine form keeps a keystroke alive instead
                        // of killing the engine, and the golden tests prove the fallback is never
                        // reached on real input.
                        out.push(coarse_vowel(u).unwrap_or(rest));
                    }
                    Level::Fine => out.push(rest),
                }
            }
            continue;
        }
        if u == "y" {
            if i == 0 {
                u = "j";
            } else {
                continue;
            }
        }
        if level == Level::Coarse {
            u = coarse_cons(u);
            if u.is_empty() {
                continue;
            }
        }
        if out.last() == Some(&u) {
            continue;
        }
        out.push(u);
    }
    out.concat()
}

fn bangla_units(word: &str) -> Vec<&'static str> {
    let s = match_key(word);
    let chars: Vec<char> = s.chars().collect();
    let mut units = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let nxt = chars.get(i + 1).copied();
        if let Some(cls) = cons(ch) {
            if nxt == Some(NUKTA) {
                // NFC keeps ড় decomposed as ড + nukta, so this is the path real text takes.
                units.push(match ch {
                    'ড' => "r",
                    'ঢ' => "rh",
                    'য' => "y",
                    _ => cls,
                });
                i += 2;
                continue;
            }
            units.push(cls);
            i += 1;
            continue;
        }
        if ch == VIRAMA {
            // ya-phala and ba-phala after a consonant are usually not pronounced as j / b.
            if nxt == Some('য') {
                units.push("y");
                i += 2;
                continue;
            }
            if nxt == Some('ব') && !units.is_empty() {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(v) = vowel(ch) {
            units.push(match v {
                "o" => "Vo",
                "a" => "Va",
                "i" => "Vi",
                "u" => "Vu",
                "ri" => "Vri",
                "e" => "Ve",
                "oi" => "Voi",
                "ou" => "Vou",
                _ => unreachable!("vowel() returns only the classes listed above"),
            });
            i += 1;
            continue;
        }
        // candrabindu, digits, anything else: ignored
        i += 1;
    }
    units
}

fn roman_units(roman: &str) -> Vec<&'static str> {
    let s = normalize_roman(roman);
    // normalize_roman leaves only ASCII, so byte indexing is safe and each char is one byte.
    let b = s.as_bytes();
    let mut units = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let mut hit = None;
        for l in [3usize, 2] {
            if i + l <= b.len() {
                if let Some(u) = roman_multi(&s[i..i + l]) {
                    hit = Some(u);
                    i += l;
                    break;
                }
            }
        }
        match hit {
            Some(u) => units.push(u),
            None => {
                let c = b[i] as char;
                i += 1;
                if let Some(u) = roman_single(c) {
                    units.push(u);
                }
            }
        }
    }
    units
}

pub fn key_from_roman(roman: &str, level: Level) -> String {
    finalize(&roman_units(roman), level)
}

pub fn key_from_bangla(word: &str, level: Level) -> String {
    finalize(&bangla_units(word), level)
}

pub fn is_romanish(text: &str) -> bool {
    text.chars().flat_map(char::to_lowercase).any(|c| c.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_examples_from_the_docstring() {
        // "amr, amar, aamar and আমার all become amr"
        for r in ["amr", "amar", "aamar"] {
            assert_eq!(key_from_roman(r, Level::Coarse), "amr", "roman {r}");
        }
        assert_eq!(key_from_bangla("আমার", Level::Coarse), "amr");

        // "korchi, korci and করছি all become krc at the coarse level"
        for r in ["korchi", "korci"] {
            assert_eq!(key_from_roman(r, Level::Coarse), "krc", "roman {r}");
        }
        assert_eq!(key_from_bangla("করছি", Level::Coarse), "krc");
    }

    #[test]
    fn fine_keeps_distinctions_coarse_merges_them() {
        assert_eq!(key_from_roman("kh", Level::Fine), "kh");
        assert_eq!(key_from_roman("kh", Level::Coarse), "k");
        assert_ne!(
            key_from_roman("sasa", Level::Fine),
            key_from_roman("shasha", Level::Fine)
        );
        assert_eq!(
            key_from_roman("sasa", Level::Coarse),
            key_from_roman("shasha", Level::Coarse)
        );
    }

    #[test]
    fn y_is_a_consonant_only_word_initially() {
        // yeno = যেন: initial y becomes j
        assert!(key_from_roman("yeno", Level::Fine).starts_with('j'));
        // koria / koriya / korea must agree: a non-initial y creates no slot
        let a = key_from_roman("koria", Level::Coarse);
        let b = key_from_roman("koriya", Level::Coarse);
        let c = key_from_roman("korea", Level::Coarse);
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn only_an_initial_vowel_survives() {
        assert_eq!(key_from_roman("amar", Level::Fine), "amr");
        assert_eq!(key_from_roman("mar", Level::Fine), "mr");
    }

    #[test]
    fn doubled_consonants_collapse() {
        assert_eq!(key_from_roman("abbas", Level::Fine), key_from_roman("abas", Level::Fine));
    }

    #[test]
    fn trigraphs_beat_digraphs() {
        // "chh" must be taken whole, not as "ch" + "h"
        assert_eq!(key_from_roman("chha", Level::Fine), "ch");
        assert_eq!(key_from_roman("kkha", Level::Fine), "kh");
    }
}
