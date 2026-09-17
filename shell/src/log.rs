//! A log that works from inside someone else's process.
//!
//! The text service runs inside Notepad, Chrome, Word -- whatever has focus. There is no console
//! and a debugger is rarely attached, so the only dependable way to see what happened is a file.
//! Each line is opened, appended and closed on its own: no handle is held across calls, so a host
//! that dies mid-way loses nothing, and no lock is taken on a keystroke path.
//!
//! Lines also go to `OutputDebugStringW`, which DebugView or a debugger picks up live.
//!
//! The file is capped. Every application that ever activates the keyboard appends to the same
//! file for as long as the machine exists, and a pilot user's disk is not ours to fill: past the
//! cap the current file becomes `shell.log.1` and a fresh one starts, so there is always the most
//! recent stretch and the one before it.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;

const MAX_BYTES: u64 = 2 * 1024 * 1024;

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("Likhi").join("shell.log"))
}

fn rotate_if_large(p: &PathBuf) {
    if let Ok(meta) = std::fs::metadata(p) {
        if meta.len() > MAX_BYTES {
            let _ = std::fs::rename(p, p.with_extension("log.1"));
        }
    }
}

pub fn write(msg: &str) {
    let pid = std::process::id();
    let mut line = String::with_capacity(msg.len() + 32);
    let _ = write!(line, "[likhi-tsf pid={pid}] {msg}");

    let mut wide: Vec<u16> = line.encode_utf16().collect();
    wide.push(b'\n' as u16);
    wide.push(0);
    unsafe { OutputDebugStringW(PCWSTR(wide.as_ptr())) };

    if let Some(p) = path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        rotate_if_large(&p);
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&p) {
            let _ = writeln!(f, "{line}");
        }
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
