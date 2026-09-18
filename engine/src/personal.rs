//! What this user picked for what they typed. A port of `src/likhi/engine/personal.py`.
//!
//! Two signals, in a small SQLite file the user owns and can export or delete:
//!
//! * selections: (normalized roman, chosen word) -> count, last used. "If I keep picking the second
//!   suggestion for a word, it becomes first." Also how English passthrough is learned: choosing
//!   the raw Latin for "ok" a few times makes Latin the top candidate for "ok".
//! * words: chosen word -> count. A personal unigram that lifts the user's own vocabulary -- names,
//!   slang, workplace words -- everywhere, and keeps words the public lexicon never saw.
//!
//! Counts decay with a 90-day half-life so old habits fade. Nothing here ever leaves the machine.
//!
//! **This reads databases the Python engine wrote.** The schema, the column types and the decay
//! formula are therefore fixed, not a design choice: a user upgrading to the Rust engine keeps
//! everything they have taught it. The DDL below is `CREATE TABLE IF NOT EXISTS` with the same text
//! as the Python, so opening an existing file changes nothing in it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

pub const HALF_LIFE_DAYS: f64 = 90.0;

pub fn default_db_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Likhi").join("personal.sqlite")
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub struct PersonalStore {
    db: Connection,
    decay: f64,
    /// Cleared for the affected key on `learn`, as in the Python: a commit invalidates one roman
    /// and one word, not the whole cache, because every committed word calls learn.
    sel_cache: HashMap<String, Vec<(String, f64)>>,
    word_cache: HashMap<String, f64>,
}

impl PersonalStore {
    pub fn open(path: Option<&Path>) -> rusqlite::Result<PersonalStore> {
        let path = path.map(PathBuf::from).unwrap_or_else(default_db_path);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let db = Connection::open(&path)?;
        // WAL so a reader is never blocked by the commit that happens on every accepted word.
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS selections (roman TEXT, word TEXT, count REAL, last REAL, PRIMARY KEY (roman, word));
             CREATE TABLE IF NOT EXISTS words (word TEXT PRIMARY KEY, count REAL, last REAL);",
        )?;
        Ok(PersonalStore {
            db,
            decay: std::f64::consts::LN_2 / (HALF_LIFE_DAYS * 86400.0),
            sel_cache: HashMap::new(),
            word_cache: HashMap::new(),
        })
    }

    /// In-memory store, for tests and for evaluation runs that must not touch a real user's data.
    pub fn in_memory() -> rusqlite::Result<PersonalStore> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS selections (roman TEXT, word TEXT, count REAL, last REAL, PRIMARY KEY (roman, word));
             CREATE TABLE IF NOT EXISTS words (word TEXT PRIMARY KEY, count REAL, last REAL);",
        )?;
        Ok(PersonalStore {
            db,
            decay: std::f64::consts::LN_2 / (HALF_LIFE_DAYS * 86400.0),
            sel_cache: HashMap::new(),
            word_cache: HashMap::new(),
        })
    }

    fn decayed(&self, count: f64, last: f64, now: f64) -> f64 {
        count * (-self.decay * (now - last).max(0.0)).exp()
    }

    /// Record a commit. `chosen` is stored canonicalised unless it is the raw Latin the user typed,
    /// which is how English passthrough stays distinguishable from a Bangla word.
    pub fn learn(&mut self, roman: &str, chosen: &str, when: Option<f64>) -> rusqlite::Result<()> {
        let r = crate::textnorm::normalize_roman(roman);
        let w = if chosen == roman {
            chosen.to_string()
        } else {
            crate::textnorm::canonical(chosen)
        };
        if r.is_empty() || w.is_empty() {
            return Ok(());
        }
        let now = when.unwrap_or_else(now_secs);

        let prev: Option<(f64, f64)> = self
            .db
            .query_row(
                "SELECT count, last FROM selections WHERE roman=? AND word=?",
                params![r, w],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        let count = match prev {
            Some((c, last)) => self.decayed(c, last, now) + 1.0,
            None => 1.0,
        };
        self.db.execute(
            "INSERT OR REPLACE INTO selections (roman, word, count, last) VALUES (?,?,?,?)",
            params![r, w, count, now],
        )?;

        let prev: Option<(f64, f64)> = self
            .db
            .query_row("SELECT count, last FROM words WHERE word=?", params![w], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .ok();
        let wcount = match prev {
            Some((c, last)) => self.decayed(c, last, now) + 1.0,
            None => 1.0,
        };
        self.db.execute(
            "INSERT OR REPLACE INTO words (word, count, last) VALUES (?,?,?)",
            params![w, wcount, now],
        )?;

        self.sel_cache.remove(&r);
        self.word_cache.remove(&w);
        Ok(())
    }

    /// Decayed counts of words chosen for this exact roman string.
    ///
    /// Returned as an ordered list rather than a map: the caller inserts these into the candidate
    /// table in this order, and candidate insertion order decides how equal scores are broken.
    pub fn selections(&mut self, roman: &str, now: Option<f64>) -> Vec<(String, f64)> {
        let r = crate::textnorm::normalize_roman(roman);
        if let Some(hit) = self.sel_cache.get(&r) {
            return hit.clone();
        }
        let now = now.unwrap_or_else(now_secs);
        let mut out = Vec::new();
        if let Ok(mut stmt) = self
            .db
            .prepare("SELECT word, count, last FROM selections WHERE roman=?")
        {
            if let Ok(rows) = stmt.query_map(params![r], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?, row.get::<_, f64>(2)?))
            }) {
                for row in rows.flatten() {
                    let (w, c, last) = row;
                    out.push((w, self.decayed(c, last, now)));
                }
            }
        }
        self.sel_cache.insert(r, out.clone());
        out
    }

    pub fn word_count(&mut self, word: &str, now: Option<f64>) -> f64 {
        if let Some(&v) = self.word_cache.get(word) {
            return v;
        }
        let now = now.unwrap_or_else(now_secs);
        let v = self
            .db
            .query_row("SELECT count, last FROM words WHERE word=?", params![word], |row| {
                Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?))
            })
            .map(|(c, last)| self.decayed(c, last, now))
            .unwrap_or(0.0);
        self.word_cache.insert(word.to_string(), v);
        v
    }

    pub fn forget_all(&mut self) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM selections", [])?;
        self.db.execute("DELETE FROM words", [])?;
        self.sel_cache.clear();
        self.word_cache.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pick_is_remembered_and_counted() {
        let mut p = PersonalStore::in_memory().unwrap();
        let t = 1_700_000_000.0;
        p.learn("ok", "ওকে", Some(t)).unwrap();
        let sel = p.selections("ok", Some(t));
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].0, "ওকে");
        assert!((sel[0].1 - 1.0).abs() < 1e-9);
        assert!((p.word_count("ওকে", Some(t)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn counts_decay_by_half_over_the_half_life() {
        let mut p = PersonalStore::in_memory().unwrap();
        let t = 1_700_000_000.0;
        p.learn("ok", "ওকে", Some(t)).unwrap();
        let later = t + HALF_LIFE_DAYS * 86400.0;
        // The cache holds the value computed at `t`, and the Python has the same property: the
        // decay is applied when the row is read fresh from the database.
        p.sel_cache.clear();
        let sel = p.selections("ok", Some(later));
        assert!((sel[0].1 - 0.5).abs() < 1e-6, "decayed to {}", sel[0].1);
    }

    #[test]
    fn repeated_picks_accumulate_on_top_of_the_decayed_count() {
        let mut p = PersonalStore::in_memory().unwrap();
        let t = 1_700_000_000.0;
        p.learn("ok", "ওকে", Some(t)).unwrap();
        let later = t + HALF_LIFE_DAYS * 86400.0;
        p.learn("ok", "ওকে", Some(later)).unwrap();
        let sel = p.selections("ok", Some(later));
        // 1.0 decayed to 0.5, plus the new pick.
        assert!((sel[0].1 - 1.5).abs() < 1e-6, "count {}", sel[0].1);
    }

    #[test]
    fn raw_latin_is_stored_as_typed_not_canonicalised() {
        let mut p = PersonalStore::in_memory().unwrap();
        let t = 1_700_000_000.0;
        p.learn("ok", "ok", Some(t)).unwrap();
        let sel = p.selections("ok", Some(t));
        assert_eq!(sel[0].0, "ok", "English passthrough must stay Latin");
    }
}
