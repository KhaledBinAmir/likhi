//! Settings, read from the same files the engine reads.
//!
//! Two layers, machine then user, merged key by key: the installed config carries the defaults, and
//! `%LOCALAPPDATA%\Likhi\config.json` holds what one person has changed. The per-user file wins and
//! needs no administrator, which is what lets the Likhi window change a setting on a machine the
//! person does not own.
//!
//! Read once when the text service activates. Nothing here is on a keystroke path.

use std::path::PathBuf;

use serde::Deserialize;

use crate::log;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server_port: u16,
    pub candidates: usize,
    /// Which key switches between Bangla and straight English. "F12", "F2", or "" for none.
    pub toggle_key: String,
    /// Digits typed while nothing is being composed become Bengali digits.
    pub bangla_digits: bool,
    /// A full stop gives the Bengali daṛi rather than a period.
    pub danda_for_period: bool,
    pub space_commits: bool,
    pub enter_commits: bool,
    /// Candidate window face. Empty means the built-in preference chain.
    pub font_name: String,
    pub font_size: f32,
    /// After a word is committed with Space, offer the words likely to come next. Tab takes the
    /// first; any other key dismisses them.
    pub next_word: bool,
    /// How sure the engine must be before a suggestion appears: the top continuation's share of
    /// everything seen after the previous word. 0.2 measured right 35% of the time in the top three,
    /// against 18% when always shown -- see `likhi-nextword`.
    pub next_word_min_share: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server_port: 47123,
            candidates: 5,
            toggle_key: "F12".into(),
            bangla_digits: true,
            danda_for_period: true,
            space_commits: true,
            enter_commits: true,
            font_name: String::new(),
            font_size: 14.0,
            next_word: true,
            next_word_min_share: 0.2,
        }
    }
}

impl Config {
    /// Machine config, then the per-user file laid over it.
    pub fn load() -> Self {
        let mut merged = serde_json::Map::new();
        for path in search_paths() {
            if let Some(map) = read_object(&path) {
                for (k, v) in map {
                    merged.insert(k, v);
                }
            }
        }
        match serde_json::from_value::<Config>(serde_json::Value::Object(merged)) {
            Ok(c) => c,
            Err(e) => {
                log!("config unreadable ({e}); using defaults");
                Config::default()
            }
        }
    }

    /// The virtual key code of the toggle key, if one is configured.
    pub fn toggle_vk(&self) -> Option<u32> {
        let name = self.toggle_key.trim().to_ascii_uppercase();
        let n: u32 = name.strip_prefix('F')?.parse().ok()?;
        if (1..=24).contains(&n) {
            Some(0x70 + n - 1) // VK_F1 is 0x70
        } else {
            None
        }
    }
}

/// Least specific first, so later files override earlier ones.
fn search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    // Where the installer puts it once this shell replaces PIME.
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        paths.push(PathBuf::from(pf).join("Likhi").join("config.json"));
    }
    // The PIME layout, which versions before 0.2.0 installed. Read for one more release so that a
    // machine upgraded from one of those keeps its settings; remove once the pilot is past them.
    if let Some(pf86) = std::env::var_os("ProgramFiles(x86)") {
        paths.push(
            PathBuf::from(pf86)
                .join("PIME")
                .join("python")
                .join("input_methods")
                .join("likhi")
                .join("config.json"),
        );
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        paths.push(PathBuf::from(local).join("Likhi").join("config.json"));
    }
    paths
}

/// Parse one file, tolerating a byte-order mark: administrators edit these in Notepad, which writes
/// one, and plain UTF-8 parsing rejects it. That silently disabled telemetry once already.
fn read_object(path: &PathBuf) -> Option<serde_json::Map<String, serde_json::Value>> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => Some(map),
        Ok(_) => None,
        Err(e) => {
            log!("ignoring {}: {e}", path.display());
            None
        }
    }
}

/// Newest modification time across the config files, as a cheap "has anything changed" stamp.
///
/// The candidate window watches this so a font chosen in the Likhi window takes effect while
/// someone is typing, rather than at the next sign-in. Two `stat` calls, taken at most once a
/// second and never on the path of a keystroke that is being handled.
pub fn stamp() -> u128 {
    // Nanoseconds, not seconds: two saves inside the same second -- pick a font, then a size --
    // would otherwise look like no change at all, and the second choice would not take.
    search_paths()
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .filter_map(|m| m.modified().ok())
        .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .max()
        .unwrap_or(0)
}

/// Bengali digits, indexed by value.
pub const BANGLA_DIGITS: [&str; 10] = ["০", "১", "২", "৩", "৪", "৫", "৬", "৭", "৮", "৯"];
