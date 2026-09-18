//! Avro Phonetic: the rule-based literal reading of a typed string.
//!
//! A port of the forward `parse()` path of the `avro.py` package (MIT OR Apache-2.0), which the
//! engine uses as its fourth candidate channel. Its value is that it always produces *something*
//! spellable: whatever the lexicon and the model think, the rule reading of what was typed stays
//! reachable, so no spelling is untypeable.
//!
//! The rules themselves are not transcribed here. They are 52 KB of Bengali strings in the original
//! and are emitted as `avro.json` by `scripts/build_rust_data.py`; this module ports the algorithm
//! and reads the tables. Only the forward direction is ported -- Bijoy conversion and reverse
//! transliteration are unused by the engine.
//!
//! Two details in the original are load-bearing and easy to lose:
//!
//! * pattern precedence *is* list order. `exact_find_in_pattern` collects every pattern matching at
//!   the cursor and the caller takes the first, so the table's ordering (longest find first) is the
//!   only thing making longest-match work. Nothing here may sort or deduplicate it.
//! * `is_exact` requires `end < len(haystack)`, strictly. An exact-match condition that would be
//!   satisfied by the very end of the string fails instead. That looks like an off-by-one, and may
//!   well be one, but it is the behaviour the pilot's suggestions were tuned against, so it is
//!   reproduced rather than corrected.

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct RuleMatch {
    #[serde(rename = "type")]
    kind: Option<String>,
    scope: Option<String>,
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Rule {
    replace: String,
    matches: Vec<RuleMatch>,
}

#[derive(Debug, Deserialize)]
struct Pattern {
    find: String,
    replace: Option<String>,
    #[serde(default)]
    rules: Option<Vec<Rule>>,
}

#[derive(Debug, Deserialize)]
struct Bundle {
    patterns: Vec<Pattern>,
    vowel: String,
    consonant: String,
    casesensitive: String,
    exceptions: Vec<(String, String)>,
}

pub struct Avro {
    /// Patterns without rules and patterns with rules, each keeping the original relative order.
    non_rule: Vec<usize>,
    rule: Vec<usize>,
    patterns: Vec<Pattern>,
    vowels: HashSet<char>,
    consonants: HashSet<char>,
    casesensitive: HashSet<char>,
    exceptions: Vec<(String, String)>,
}

impl Avro {
    pub fn open(path: &Path) -> Result<Avro, Box<dyn std::error::Error>> {
        let text = std::fs::read_to_string(path)?;
        let b: Bundle = serde_json::from_str(&text)?;
        let non_rule = (0..b.patterns.len())
            .filter(|&i| b.patterns[i].rules.is_none())
            .collect();
        let rule = (0..b.patterns.len())
            .filter(|&i| b.patterns[i].rules.is_some())
            .collect();
        Ok(Avro {
            non_rule,
            rule,
            vowels: b.vowel.chars().collect(),
            consonants: b.consonant.chars().collect(),
            casesensitive: b.casesensitive.chars().collect(),
            exceptions: b.exceptions,
            patterns: b.patterns,
        })
    }

    fn is_vowel(&self, c: char) -> bool {
        c.to_lowercase().any(|l| self.vowels.contains(&l))
    }

    fn is_consonant(&self, c: char) -> bool {
        c.to_lowercase().any(|l| self.consonants.contains(&l))
    }

    /// Anything that is neither a vowel nor a consonant, which is how the original defines it.
    fn is_punctuation(&self, c: char) -> bool {
        !(self.is_vowel(c) || self.is_consonant(c))
    }

    /// Lowercase every character except the ones whose case the rules rely on.
    fn fix_string_case(&self, text: &str) -> String {
        text.chars()
            .flat_map(|c| {
                if c.to_lowercase().any(|l| self.casesensitive.contains(&l)) {
                    vec![c]
                } else {
                    c.to_lowercase().collect::<Vec<_>>()
                }
            })
            .collect()
    }

    /// Wrap known exception words in `<rm>` markers so they pass through unparsed.
    ///
    /// Each exception is applied to the text the previous one produced, left to right and
    /// non-overlapping, which is what `re.sub` does; a later pattern can therefore match inside an
    /// earlier substitution's marker, and that is the original's behaviour.
    fn find_in_remap(&self, text: &str) -> (String, bool) {
        let mut text = text.to_string();
        for (key, value) in &self.exceptions {
            let needle = value.to_lowercase();
            if needle.is_empty() {
                continue;
            }
            let mut out = String::with_capacity(text.len());
            let bytes = text.as_bytes();
            let mut i = 0usize;
            while i < text.len() {
                if !text.is_char_boundary(i) {
                    i += 1;
                    continue;
                }
                let end = i + needle.len();
                let hit = end <= text.len()
                    && text.is_char_boundary(end)
                    && bytes[i..end].eq_ignore_ascii_case(needle.as_bytes());
                if hit {
                    out.push_str("<rm>");
                    out.push_str(key);
                    out.push_str("</rm>");
                    i = end;
                } else {
                    let ch = text[i..].chars().next().expect("boundary");
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            text = out;
        }
        let manual_required = split_markers(&text)
            .iter()
            .any(|seg| !seg.is_empty() && !(seg.starts_with("<rm>") && seg.ends_with("</rm>")));
        (text, manual_required)
    }

    /// The first pattern whose `find` matches at `cur`, searching the given index list in order.
    fn exact_find(&self, text: &[char], cur: usize, which: &[usize]) -> Option<&Pattern> {
        for &i in which {
            let p = &self.patterns[i];
            let f: Vec<char> = p.find.chars().collect();
            if cur + f.len() <= text.len() && text[cur..cur + f.len()] == f[..] {
                return Some(p);
            }
        }
        None
    }

    /// `process_rules`: the first rule all of whose matches hold wins.
    fn process_rules(&self, rules: &[Rule], text: &[char], cur: usize, cur_end: usize) -> Option<String> {
        for rule in rules {
            let mut matched = true;
            for m in &rule.matches {
                matched = self.process_match(m, text, cur, cur_end);
                if !matched {
                    break;
                }
            }
            if matched {
                return Some(rule.replace.clone());
            }
        }
        None
    }

    fn process_match(&self, m: &RuleMatch, text: &[char], cur: usize, cur_end: usize) -> bool {
        let (Some(kind), Some(scope_raw)) = (m.kind.as_deref(), m.scope.as_deref()) else {
            return false;
        };
        let prefix = kind == "prefix";
        // chk is signed: a prefix check at the start of the string looks at position -1, and the
        // scope tests below distinguish "off the front" from "a character that is not a vowel".
        let chk: isize = if prefix { cur as isize - 1 } else { cur_end as isize };
        let (scope, negative) = match scope_raw.strip_prefix('!') {
            Some(rest) => (rest, true),
            None => (scope_raw, false),
        };
        let len = text.len() as isize;
        let at = |i: isize| -> Option<char> {
            if i >= 0 && i < len {
                Some(text[i as usize])
            } else {
                None
            }
        };

        let condition = match scope {
            "punctuation" => {
                (chk < 0 && prefix)
                    || (chk >= len && !prefix)
                    || at(chk).map(|c| self.is_punctuation(c)).unwrap_or(false)
            }
            "vowel" => {
                ((chk >= 0 && prefix) || (chk < len && !prefix))
                    && at(chk).map(|c| self.is_vowel(c)).unwrap_or(false)
            }
            "consonant" => {
                ((chk >= 0 && prefix) || (chk < len && !prefix))
                    && at(chk).map(|c| self.is_consonant(c)).unwrap_or(false)
            }
            "exact" => {
                let Some(value) = m.value.as_deref() else {
                    return false;
                };
                let v: Vec<char> = value.chars().collect();
                let (start, end) = if prefix {
                    (cur as isize - v.len() as isize, cur as isize)
                } else {
                    (cur_end as isize, cur_end as isize + v.len() as isize)
                };
                // `end < len`, strictly, exactly as is_exact does. See the module note.
                let found = start >= 0
                    && end < len
                    && text[start as usize..end as usize] == v[..];
                // is_exact folds the negation in itself and returns the final verdict, so the
                // caller's `!= negative` is not applied again here.
                return found != negative;
            }
            _ => return true, // an unknown scope leaves `replace` at its default of true
        };
        // `if not condition != negative: replace = False`, i.e. the match holds when
        // `(!condition) == negative`.
        (!condition) == negative
    }

    /// Parse a roman string into Bengali. Equivalent to `avro.parse(text)` with its defaults.
    pub fn parse(&self, text: &str) -> String {
        let fixed = self.fix_string_case(text);
        let (remapped, manual_required) = self.find_in_remap(&fixed);
        if !manual_required {
            return remapped.replace("<rm>", "").replace("</rm>", "");
        }
        let mut out = String::with_capacity(remapped.len());
        for seg in split_markers(&remapped) {
            if seg.starts_with("<rm>") && seg.ends_with("</rm>") {
                out.push_str(&seg[4..seg.len() - 5]);
            } else if !seg.is_empty() {
                out.push_str(&self.parse_segment(&seg));
            }
        }
        out
    }

    fn parse_segment(&self, segment: &str) -> String {
        let text: Vec<char> = segment.chars().collect();
        let mut out = String::with_capacity(segment.len() * 3);
        let mut cur_end = 0usize;

        for cur in 0..text.len() {
            let ch = text[cur];
            if (ch as u32) >= 128 {
                cur_end = cur + 1;
                out.push(ch);
                continue;
            }
            if cur < cur_end {
                continue;
            }
            // Non-rule patterns first. A pattern with no "replace" does not substitute: the
            // original tests that the replacement is a string, so a missing one falls through to
            // the rule table rather than inserting nothing.
            if let Some(p) = self.exact_find(&text, cur, &self.non_rule) {
                if let Some(rep) = p.replace.as_deref() {
                    out.push_str(rep);
                    cur_end = cur + p.find.chars().count();
                    continue;
                }
            }
            if let Some(p) = self.exact_find(&text, cur, &self.rule) {
                cur_end = cur + p.find.chars().count();
                let rules = p.rules.as_deref().unwrap_or(&[]);
                match self.process_rules(rules, &text, cur, cur_end) {
                    Some(v) if !v.is_empty() => out.push_str(&v),
                    _ => {
                        if let Some(rep) = p.replace.as_deref() {
                            out.push_str(rep);
                        }
                    }
                }
                continue;
            }
            cur_end = cur + 1;
            out.push(ch);
        }
        out
    }
}

/// `re.split(r"(<rm>.*?</rm>)", text)` -- the separators are kept, and `.` does not cross newlines.
fn split_markers(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find("<rm>") else {
            out.push(rest.to_string());
            return out;
        };
        // Non-greedy: the nearest closing marker. A newline between them stops the match, because
        // `.` excludes newlines without re.DOTALL.
        let after = &rest[start + 4..];
        let Some(rel_end) = after.find("</rm>") else {
            out.push(rest.to_string());
            return out;
        };
        if after[..rel_end].contains('\n') {
            out.push(rest.to_string());
            return out;
        }
        let end = start + 4 + rel_end + 5;
        out.push(rest[..start].to_string());
        out.push(rest[start..end].to_string());
        rest = &rest[end..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_splitting_matches_the_python_regex() {
        // re.split keeps the captured separators and produces the surrounding pieces, including
        // empty ones at the ends.
        assert_eq!(split_markers("a<rm>X</rm>b"), vec!["a", "<rm>X</rm>", "b"]);
        assert_eq!(split_markers("<rm>X</rm>"), vec!["", "<rm>X</rm>", ""]);
        assert_eq!(split_markers("plain"), vec!["plain"]);
        // Non-greedy: the first closing marker ends the match.
        assert_eq!(
            split_markers("<rm>A</rm><rm>B</rm>"),
            vec!["", "<rm>A</rm>", "", "<rm>B</rm>", ""]
        );
    }
}
