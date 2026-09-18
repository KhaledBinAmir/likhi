//! The typing habits the stress tool rewrites romanizations into.
//! A port of the `STYLES` table in `src/likhi/eval/stress.py`.
//!
//! The point of the tool is that people do not type the attested spelling. They drop vowels, double
//! them, write c for ch, mistype a neighbouring key. Each style rewrites a known-good romanization
//! the way some group of users would, and the tool then reports top-1 per habit -- which is how you
//! find out that the engine is fine on Dakshina and poor on how anyone actually types.
//!
//! The randomised styles draw from `pyrandom`, CPython's generator, so a given seed produces the
//! same variants as the Python tool and historical stress results stay comparable.

use super::pyrandom::PyRandom;

/// Names in the order the Python dictionary defines them. Order matters: the tool applies every
/// style to each word in turn from one generator, so a different order changes every draw after it.
pub const STYLE_NAMES: [&str; 11] = [
    "as_is",
    "drop_vowels",
    "double",
    "a_for_o",
    "c_for_ch",
    "s_for_sh",
    "y_for_j",
    "v_for_bh",
    "caps",
    "typo",
    "no_h",
];

fn qwerty_neighbours(c: char) -> Option<&'static str> {
    Some(match c {
        'q' => "wa",
        'w' => "qes",
        'e' => "wrd",
        'r' => "etf",
        't' => "ryg",
        'y' => "tuh",
        'u' => "yij",
        'i' => "uok",
        'o' => "ipl",
        'p' => "o",
        'a' => "qsz",
        's' => "awdz",
        'd' => "sefx",
        'f' => "drgc",
        'g' => "fthv",
        'h' => "gyjb",
        'j' => "hukn",
        'k' => "jilm",
        'l' => "kop",
        'z' => "asx",
        'x' => "zsdc",
        'c' => "xdfv",
        'v' => "cfgb",
        'b' => "vghn",
        'n' => "bhjm",
        'm' => "njk",
        _ => return None,
    })
}

/// Interior a/o dropped with probability 0.7; first and last characters always kept.
fn drop_vowels(r: &str, rng: &mut PyRandom) -> String {
    let chars: Vec<char> = r.chars().collect();
    if chars.len() < 4 {
        return r.to_string();
    }
    let mut out = String::new();
    out.push(chars[0]);
    for &ch in &chars[1..chars.len() - 1] {
        if (ch == 'a' || ch == 'o') && rng.random() < 0.7 {
            continue;
        }
        out.push(ch);
    }
    out.push(chars[chars.len() - 1]);
    out
}

/// Long vowels doubled: each a/i/u repeated with probability 0.5.
fn double(r: &str, rng: &mut PyRandom) -> String {
    let mut out = String::new();
    for ch in r.chars() {
        out.push(ch);
        if matches!(ch, 'a' | 'i' | 'u') && rng.random() < 0.5 {
            out.push(ch);
        }
    }
    out
}

/// o written as a and vice versa, in the middle of the word only.
fn a_for_o(r: &str, rng: &mut PyRandom) -> String {
    let chars: Vec<char> = r.chars().collect();
    if chars.len() < 3 {
        return r.to_string();
    }
    let mut core: Vec<char> = chars[1..chars.len() - 1].to_vec();
    for ch in core.iter_mut() {
        if *ch == 'o' && rng.random() < 0.6 {
            *ch = 'a';
        } else if *ch == 'a' && rng.random() < 0.3 {
            *ch = 'o';
        }
    }
    let mut out = String::new();
    out.push(chars[0]);
    out.extend(core);
    out.push(chars[chars.len() - 1]);
    out
}

fn c_for_ch(r: &str) -> String {
    r.replace("chh", "ch").replace("ch", "c")
}

fn s_for_sh(r: &str) -> String {
    r.replace("sh", "s").replace("ss", "s")
}

/// j written as y or z, jh flattened to j first.
fn y_for_j(r: &str, rng: &mut PyRandom) -> String {
    let flattened = r.replace("jh", "j");
    let yz: Vec<char> = "yz".chars().collect();
    let mut out = String::new();
    for ch in flattened.chars() {
        if ch == 'j' {
            out.push(*rng.choice(&yz));
        } else {
            out.push(ch);
        }
    }
    out
}

fn v_for_bh(r: &str) -> String {
    r.replace("bh", "v").replace("ph", "f").replace('w', "o")
}

/// Random capitalisation: each letter uppercased with probability 0.3.
fn caps(r: &str, rng: &mut PyRandom) -> String {
    r.chars()
        .map(|ch| {
            if rng.random() < 0.3 {
                ch.to_uppercase().next().unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect()
}

/// One adjacent-key substitution, never at the first or last position.
fn typo(r: &str, rng: &mut PyRandom) -> String {
    let chars: Vec<char> = r.chars().collect();
    if chars.len() < 4 {
        return r.to_string();
    }
    let i = rng.randrange(1, chars.len() - 1);
    let Some(nb) = qwerty_neighbours(chars[i]) else {
        return r.to_string();
    };
    let options: Vec<char> = nb.chars().collect();
    let mut out: Vec<char> = chars.clone();
    out[i] = *rng.choice(&options);
    out.into_iter().collect()
}

/// h dropped after the consonants that take an aspirated form.
fn no_h(r: &str) -> String {
    let chars: Vec<char> = r.chars().collect();
    let mut out = String::new();
    for (i, &ch) in chars.iter().enumerate() {
        if ch == 'h' && i > 0 && matches!(chars[i - 1], 'k' | 'g' | 'c' | 'j' | 't' | 'd' | 'p' | 'b')
        {
            continue;
        }
        out.push(ch);
    }
    out
}

/// Rewrite `roman` in the named style. An unknown name returns the input unchanged.
///
/// The deterministic styles ignore `rng` and, importantly, draw nothing from it: the Python's are
/// plain string replacements too, so consuming a draw here would desynchronise every style after it.
pub fn apply(style: &str, roman: &str, rng: &mut PyRandom) -> String {
    match style {
        "as_is" => roman.to_string(),
        "drop_vowels" => drop_vowels(roman, rng),
        "double" => double(roman, rng),
        "a_for_o" => a_for_o(roman, rng),
        "c_for_ch" => c_for_ch(roman),
        "s_for_sh" => s_for_sh(roman),
        "y_for_j" => y_for_j(roman, rng),
        "v_for_bh" => v_for_bh(roman),
        "caps" => caps(roman, rng),
        "typo" => typo(roman, rng),
        "no_h" => no_h(roman),
        _ => roman.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng() -> PyRandom {
        PyRandom::new(13)
    }

    #[test]
    fn deterministic_styles_need_no_randomness() {
        let mut r = rng();
        assert_eq!(apply("as_is", "korchi", &mut r), "korchi");
        assert_eq!(apply("c_for_ch", "korchi", &mut r), "korci");
        assert_eq!(apply("c_for_ch", "bachcha", &mut r), "bacca");
        assert_eq!(apply("s_for_sh", "shopno", &mut r), "sopno");
        assert_eq!(apply("v_for_bh", "bhalo", &mut r), "valo");
        assert_eq!(apply("v_for_bh", "phul", &mut r), "ful");
        assert_eq!(apply("v_for_bh", "w", &mut r), "o");
        assert_eq!(apply("no_h", "khub", &mut r), "kub");
        assert_eq!(apply("no_h", "dhonnobad", &mut r), "donnobad");
        // A leading h has no consonant before it and must survive.
        assert_eq!(apply("no_h", "halka", &mut r), "halka");
    }

    #[test]
    fn deterministic_styles_draw_nothing() {
        // If a deterministic style consumed a draw, every later style would desynchronise from the
        // Python's sequence and the whole tool would silently measure different variants.
        let mut a = rng();
        let before = a.random();
        let mut b = rng();
        let _ = apply("c_for_ch", "korchi", &mut b);
        let _ = apply("s_for_sh", "shopno", &mut b);
        let _ = apply("v_for_bh", "bhalo", &mut b);
        let _ = apply("no_h", "khub", &mut b);
        let _ = apply("as_is", "amar", &mut b);
        assert_eq!(before, b.random());
    }

    #[test]
    fn short_words_are_left_alone_by_the_length_guarded_styles() {
        let mut r = rng();
        assert_eq!(apply("drop_vowels", "ami", &mut r), "ami", "shorter than 4");
        assert_eq!(apply("typo", "ami", &mut r), "ami", "shorter than 4");
        assert_eq!(apply("a_for_o", "ao", &mut r), "ao", "shorter than 3");
    }

    #[test]
    fn randomised_styles_keep_the_ends_intact() {
        let mut r = rng();
        for _ in 0..50 {
            let v = apply("drop_vowels", "amaroto", &mut r);
            assert!(v.starts_with('a') && v.ends_with('o'), "{v}");
        }
        let mut r = rng();
        for _ in 0..50 {
            let v = apply("typo", "korchi", &mut r);
            assert_eq!(v.len(), "korchi".len());
            assert!(v.starts_with('k') && v.ends_with('i'), "{v}");
        }
    }

    #[test]
    fn y_for_j_flattens_jh_first() {
        let mut r = rng();
        let v = apply("y_for_j", "jhal", &mut r);
        assert!(v == "yal" || v == "zal", "{v}");
    }

    /// The whole sequence, against CPython.
    ///
    /// Every style is applied to every word from one `Random(13)`, in the order `stress.py` uses.
    /// Checking the functions one at a time would not catch the failure that matters: a style that
    /// consumes a different number of draws than its Python counterpart leaves every later variant
    /// different while each function still looks correct on its own.
    ///
    /// Printed by CPython and transcribed.
    #[test]
    fn the_whole_sequence_matches_cpython() {
        const WORDS: [&str; 8] = [
            "korchi", "amar", "bhalobasha", "dhonnobad", "jhogra", "shopno", "khub", "tumi",
        ];
        #[rustfmt::skip]
        const WANT: [[&str; 11]; 8] = [
            ["korchi", "krchi", "korchi", "korchi", "korci", "korchi", "korchi", "korchi", "kORCHi", "koechi", "korci"],
            ["amar", "amr", "aamaar", "amar", "amar", "amar", "amar", "amar", "amAR", "anar", "amar"],
            ["bhalobasha", "bhalobasha", "bhalobasha", "bhalabasha", "bhalobasha", "bhalobasa", "bhalobasha", "valobasha", "bhalobaSha", "bhalonasha", "balobasha"],
            ["dhonnobad", "dhnnobd", "dhonnobad", "dhannabod", "dhonnobad", "dhonnobad", "dhonnobad", "dhonnobad", "dHonnObAD", "dhlnnobad", "donnobad"],
            ["jhogra", "jhgra", "jhograa", "jhogra", "jhogra", "jhogra", "zogra", "jhogra", "jhoGRa", "jhogfa", "jogra"],
            ["shopno", "shpno", "shopno", "shopno", "shopno", "sopno", "shopno", "shopno", "sHopno", "shlpno", "shopno"],
            ["khub", "khub", "khub", "khub", "khub", "khub", "khub", "khub", "kHub", "kjub", "kub"],
            ["tumi", "tumi", "tumi", "tumi", "tumi", "tumi", "tumi", "tumi", "TUmi", "tymi", "tumi"],
        ];

        let mut r = PyRandom::new(13);
        for (w, want_row) in WORDS.iter().zip(WANT.iter()) {
            for (style, want) in STYLE_NAMES.iter().zip(want_row.iter()) {
                let got = apply(style, w, &mut r);
                assert_eq!(&got, want, "{style}({w})");
            }
        }
    }
}
