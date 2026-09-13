//! A log file, because the daemon has no console.
//!
//! The console binary reports failures to stderr and the whole tool is built
//! around never failing quietly. The tray daemon is a GUI-subsystem process
//! with nowhere to print, so without this a CEC timeout or a refused
//! `SetDisplayConfig` during a hotkey action would vanish — the exact outcome
//! the rest of the code goes out of its way to prevent. The tray's "Open log
//! file" is the other half of the arrangement.
//!
//! Lines go to stdout as well. That costs nothing when no console is attached,
//! and keeps `monitor-switcher-tray` readable if you do run it from a prompt.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Rotate at a megabyte, keeping one older generation. Steady state is a couple
/// of lines per hotkey press, so the only way to reach this is something going
/// wrong repeatedly — which is when the recent lines are the ones worth having.
const MAX_BYTES: u64 = 1 << 20;

static SINK: OnceLock<Mutex<PathBuf>> = OnceLock::new();

/// Point the log at a file. Called once, at startup.
pub fn to_file(path: PathBuf) {
    let _ = SINK.set(Mutex::new(path));
}

/// Where the log is, for the tray's "Open log file".
pub fn path() -> Option<PathBuf> {
    Some(SINK.get()?.lock().ok()?.clone())
}

/// The log file that sits beside a config.
pub fn beside(config: &Path) -> PathBuf {
    config.with_file_name("tray.log")
}

#[doc(hidden)]
pub fn write(line: &str) {
    // stdout gets the line exactly as the CLI has always printed it — no
    // timestamp. `list --json` goes through here too, and a prefix would make
    // it something no JSON parser would accept. The file, which nothing parses,
    // gets the time.
    println!("{line}");

    let Some(sink) = SINK.get() else {
        return;
    };
    let Ok(path) = sink.lock() else {
        return;
    };
    rotate(&path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // A byte-order mark on a fresh file. The messages that end up here are full
    // of em dashes, and without a BOM both Notepad and PowerShell read the file
    // as the system code page and render them as mojibake — a poor look for the
    // file you open precisely to find out what went wrong.
    let fresh = !matches!(std::fs::metadata(&*path), Ok(m) if m.len() > 0);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&*path) {
        if fresh {
            let _ = f.write_all(&[0xEF, 0xBB, 0xBF]);
        }
        let _ = writeln!(f, "{} {line}", now());
    }
}

fn rotate(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() < MAX_BYTES {
        return;
    }
    let _ = std::fs::rename(path, path.with_extension("log.1"));
}

/// A timestamp without pulling in a date library: the log only has to say when,
/// well enough to line up against something else that happened.
fn now() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime only writes the SYSTEMTIME it is given.
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

/// Write a line to the log and to stdout.
#[macro_export]
macro_rules! log {
    () => { $crate::log::write("") };
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
