//! Metrics for transliteration candidates. A port of `src/likhi/eval/metrics.py`.
//!
//! Every comparison goes through `textnorm::match_key`, so nukta encoding, joiners and candrabindu
//! ordering never count as errors. Gold may be a set of acceptable spellings (জন্য / জন্যে).
//!
//! These numbers are the gate on every ranking change, so they are ported exactly, including the
//! parts that look like rounding trivia: `percentile` reproduces Python's nearest-rank formula and
//! its banker's rounding, because a p95 that differs in the last place makes two runs look
//! different when nothing changed.

use crate::textnorm::match_key;

/// Levenshtein distance over characters, not bytes -- the Python iterates a `str`, and a Bengali
/// character is three bytes, so a byte-wise distance would inflate every CER roughly threefold.
pub fn edit_distance(a: &[char], b: &[char]) -> usize {
    if a == b {
        return 0;
    }
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j + 1] + 1)
                .min(cur[j] + 1)
                .min(prev[j] + usize::from(ca != cb));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Character error rate of `hyp` against `ref`: 0 when equal, and it may exceed 1.
pub fn cer(hyp: &str, reference: &str) -> f64 {
    let h: Vec<char> = match_key(hyp).chars().collect();
    let r: Vec<char> = match_key(reference).chars().collect();
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&h, &r) as f64 / r.len() as f64
}

/// 1-based rank of the first candidate equal to any gold, or None.
pub fn rank_of_gold(candidates: &[String], golds: &[String]) -> Option<usize> {
    let gold_keys: std::collections::HashSet<String> =
        golds.iter().map(|g| match_key(g)).collect();
    candidates
        .iter()
        .position(|c| gold_keys.contains(&match_key(c)))
        .map(|i| i + 1)
}

/// Accumulates word-level results, optionally weighted (Dakshina attestation counts).
pub struct WordEval {
    ks: Vec<usize>,
    weight_total: f64,
    hits: std::collections::BTreeMap<usize, f64>,
    rr_total: f64,
    cer_total: f64,
    n: usize,
}

impl WordEval {
    pub fn new(mut ks: Vec<usize>) -> WordEval {
        ks.sort_unstable();
        ks.dedup();
        WordEval {
            ks,
            weight_total: 0.0,
            hits: std::collections::BTreeMap::new(),
            rr_total: 0.0,
            cer_total: 0.0,
            n: 0,
        }
    }

    pub fn add(&mut self, candidates: &[String], golds: &[String], weight: f64) -> Option<usize> {
        let rank = rank_of_gold(candidates, golds);
        self.n += 1;
        self.weight_total += weight;
        for &k in &self.ks {
            if rank.is_some_and(|r| r <= k) {
                *self.hits.entry(k).or_insert(0.0) += weight;
            }
        }
        if let Some(r) = rank {
            self.rr_total += weight / r as f64;
        }
        let top1 = candidates.first().map(String::as_str).unwrap_or("");
        let best = golds
            .iter()
            .map(|g| cer(top1, g))
            .fold(f64::INFINITY, f64::min);
        // `min()` over an empty gold list is an error in Python; here it would be +inf, which would
        // silently poison the total. An item with no gold is not evaluable.
        if best.is_finite() {
            self.cer_total += weight * best;
        }
        rank
    }

    /// `(n, [(k, topk)], mrr, cer)` -- the same fields the Python's `summary()` produces.
    pub fn summary(&self) -> Summary {
        if self.weight_total == 0.0 {
            return Summary { n: 0, topk: Vec::new(), mrr: 0.0, cer: 0.0 };
        }
        Summary {
            n: self.n,
            topk: self
                .ks
                .iter()
                .map(|&k| (k, 100.0 * self.hits.get(&k).copied().unwrap_or(0.0) / self.weight_total))
                .collect(),
            mrr: self.rr_total / self.weight_total,
            cer: 100.0 * self.cer_total / self.weight_total,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub n: usize,
    pub topk: Vec<(usize, f64)>,
    pub mrr: f64,
    pub cer: f64,
}

impl Summary {
    pub fn top(&self, k: usize) -> f64 {
        self.topk.iter().find(|(kk, _)| *kk == k).map(|(_, v)| *v).unwrap_or(0.0)
    }
}

/// Python's `round()`: half away from zero is what `f64::round` does, but Python rounds half to
/// even. Only exact ties differ, and `percentile` hits them routinely because its argument is
/// `p/100 * n + 0.5`.
fn python_round(x: f64) -> f64 {
    let floor = x.floor();
    let frac = x - floor;
    if (frac - 0.5).abs() < f64::EPSILON {
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        x.round()
    }
}

/// Nearest-rank percentile, `p` in 0..100.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut s = values.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let raw = python_round(p / 100.0 * s.len() as f64 + 0.5) - 1.0;
    let k = raw.max(0.0).min(s.len() as f64 - 1.0) as usize;
    s[k]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance(&chars("abc"), &chars("abc")), 0);
        assert_eq!(edit_distance(&chars(""), &chars("abc")), 3);
        assert_eq!(edit_distance(&chars("abc"), &chars("")), 3);
        assert_eq!(edit_distance(&chars("kitten"), &chars("sitting")), 3);
    }

    #[test]
    fn cer_is_measured_in_characters_not_bytes() {
        // Two Bengali words differing by one character. Measured in bytes this would be 3/N.
        let a = "আমার";
        let b = "আমরা";
        let d = cer(a, b);
        assert!((d - 2.0 / 4.0).abs() < 1e-12, "cer = {d}");
        assert_eq!(cer(a, a), 0.0);
    }

    #[test]
    fn cer_normalises_before_comparing() {
        // Precomposed and decomposed nukta are the same word and must not count as an error.
        assert_eq!(cer("\u{09DC}", "\u{09A1}\u{09BC}"), 0.0);
    }

    #[test]
    fn rank_is_one_based_and_normalised() {
        let cands: Vec<String> = ["আমি", "\u{09DC}", "আমার"].iter().map(|s| s.to_string()).collect();
        assert_eq!(rank_of_gold(&cands, &["আমার".to_string()]), Some(3));
        assert_eq!(rank_of_gold(&cands, &["\u{09A1}\u{09BC}".to_string()]), Some(2));
        assert_eq!(rank_of_gold(&cands, &["তুমি".to_string()]), None);
    }

    #[test]
    fn weighted_topk_and_mrr() {
        let mut ev = WordEval::new(vec![1, 3, 5]);
        let c: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        ev.add(&c, &["a".to_string()], 3.0); // rank 1
        ev.add(&c, &["c".to_string()], 1.0); // rank 3
        let s = ev.summary();
        assert_eq!(s.n, 2);
        assert!((s.top(1) - 75.0).abs() < 1e-9, "top1 = {}", s.top(1));
        assert!((s.top(3) - 100.0).abs() < 1e-9);
        // (3*1 + 1*(1/3)) / 4
        assert!((s.mrr - (3.0 + 1.0 / 3.0) / 4.0).abs() < 1e-12, "mrr = {}", s.mrr);
    }

    /// Every expected value below was produced by running `likhi.eval.metrics.percentile` in
    /// CPython, not derived by hand. The p50 of 1..10 is 6.0 and not 5.0, because the formula is
    /// `round(p/100*n + 0.5) - 1` and Python rounds 5.5 to 6; deriving these by hand got that
    /// wrong, which is why they are transcribed from the reference instead.
    #[test]
    fn percentile_matches_the_python_formula() {
        let v: Vec<f64> = (1..=10).map(|i| i as f64).collect();
        for (p, want) in [
            (0.0, 1.0),
            (25.0, 3.0),
            (50.0, 6.0),
            (75.0, 8.0),
            (95.0, 10.0),
            (99.0, 10.0),
            (100.0, 10.0),
        ] {
            assert_eq!(percentile(&v, p), want, "percentile(1..10, {p})");
        }
        // An odd length exercises the other side of the tie.
        let w: Vec<f64> = (1..=9).map(|i| i as f64).collect();
        assert_eq!(percentile(&w, 50.0), 5.0);
        assert_eq!(percentile(&w, 95.0), 9.0);

        assert_eq!(percentile(&[], 50.0), 0.0);
        assert_eq!(percentile(&[7.0], 99.0), 7.0);
    }

    #[test]
    fn python_round_is_half_to_even() {
        assert_eq!(python_round(0.5), 0.0);
        assert_eq!(python_round(1.5), 2.0);
        assert_eq!(python_round(2.5), 2.0);
        assert_eq!(python_round(3.5), 4.0);
        assert_eq!(python_round(2.4), 2.0);
        assert_eq!(python_round(2.6), 3.0);
    }
}
