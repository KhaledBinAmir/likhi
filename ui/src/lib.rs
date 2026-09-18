//! The candidate window, and nothing else.
//!
//! It lives in its own crate because it is drawn from two processes. Inside an ordinary application
//! the text service draws it directly, which is immediate and needs no coordination. Inside a
//! sandboxed one it cannot: a window created by a process in an AppContainer never reaches the
//! desktop -- `CreateWindowExW` succeeds, `SetWindowPos` succeeds, and nothing appears -- so there
//! the engine draws the same window out of process and the text service tells it what to show.
//!
//! Everything this needs from its host is injected, because the two hosts have nothing else in
//! common: the module to register the window class against, where to read the font settings, and
//! where to write log lines.

use std::sync::OnceLock;

pub mod window;

pub use window::{CandidateWindow, Content, Refiner};

/// The parts of the configuration the window actually reads. Deliberately not the host's whole
/// config type: the text service and the engine discover configuration differently, and the window
/// should not have an opinion about which is right.
///
/// Named for fonts rather than for the theme because the window already has a `Theme`, which is the
/// light or dark colour set it reads from the system.
#[derive(Clone, Debug, PartialEq)]
pub struct Fonts {
    pub font_name: String,
    pub font_size: f32,
}

impl Default for Fonts {
    fn default() -> Self {
        Fonts { font_name: String::new(), font_size: 14.0 }
    }
}

/// Where the window gets its font settings, and a stamp that changes when they do.
///
/// A closure rather than a value so the window can pick up a settings change while it is alive,
/// without the host having to know when to push one.
pub type FontSource = Box<dyn Fn() -> (u128, Fonts)>;

static LOGGER: OnceLock<fn(&str)> = OnceLock::new();
static VERBOSE: OnceLock<fn(&str)> = OnceLock::new();

/// Point this crate's logging at the host's. Calling it twice is harmless and the first call wins;
/// not calling it at all means the window simply does not log.
pub fn set_loggers(normal: fn(&str), verbose: fn(&str)) {
    let _ = LOGGER.set(normal);
    let _ = VERBOSE.set(verbose);
}

pub(crate) fn log_line(msg: &str) {
    if let Some(f) = LOGGER.get() {
        f(msg);
    }
}

pub(crate) fn vlog_line(msg: &str) {
    if let Some(f) = VERBOSE.get() {
        f(msg);
    }
}

macro_rules! log {
    ($($arg:tt)*) => { $crate::log_line(&format!($($arg)*)) };
}

macro_rules! vlog {
    ($($arg:tt)*) => { $crate::vlog_line(&format!($($arg)*)) };
}

pub(crate) use {log, vlog};
