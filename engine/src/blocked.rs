//! Words never offered as a suggestion: obscenities and slurs.
//!
//! Real chat is full of them -- the text the word-pair table and the neural model learn from
//! includes abuse by the hundred thousand -- and a keyboard that offers one unprompted is a
//! keyboard people stop trusting. Typing them is untouched: this only keeps them out of what
//! Likhi *suggests*, next words and predictions alike.
//!
//! The list is `data/blocked_suggestions.txt`, compiled in. A line is a root matched anywhere in a
//! word; a line starting with `=` is one exact word, for roots whose letters also begin innocent
//! words (বাল is blocked, বালিশ -- pillow -- is not).

use std::sync::OnceLock;

const LIST: &str = include_str!("../data/blocked_suggestions.txt");

struct Blocked {
    roots: Vec<String>,
    exact: Vec<String>,
}

fn list() -> &'static Blocked {
    static LIST_PARSED: OnceLock<Blocked> = OnceLock::new();
    LIST_PARSED.get_or_init(|| {
        let mut roots = Vec::new();
        let mut exact = Vec::new();
        for line in LIST.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match line.strip_prefix('=') {
                Some(w) => exact.push(w.to_string()),
                None => roots.push(line.to_string()),
            }
        }
        Blocked { roots, exact }
    })
}

/// Whether `word` must never be offered as a suggestion.
pub fn is_blocked(word: &str) -> bool {
    let b = list();
    b.exact.iter().any(|w| w == word) || b.roots.iter().any(|r| word.contains(r.as_str()))
}

#[cfg(test)]
mod tests {
    use super::is_blocked;

    #[test]
    fn a_root_is_caught_inside_a_word_and_an_exact_word_only_alone() {
        // Built from code points so this file carries no slur in plain sight.
        let s = |cps: &[u32]| cps.iter().map(|c| char::from_u32(*c).expect("char")).collect::<String>();
        let root_word = s(&[0x09AE, 0x09BE, 0x0997, 0x09BF, 0x09B0]); // a root with a suffix
        let exact = s(&[0x09AC, 0x09BE, 0x09B2]);
        let pillow = s(&[0x09AC, 0x09BE, 0x09B2, 0x09BF, 0x09B6]); // বালিশ, innocent
        assert!(is_blocked(&root_word));
        assert!(is_blocked(&exact));
        assert!(!is_blocked(&pillow));
        assert!(!is_blocked("আমার"));
    }
}
