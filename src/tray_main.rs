//! The tray daemon: a GUI-subsystem binary, so it owns no console and flashes
//! no window when Windows starts it at sign-in.
//!
//! Which is also why failing here is awkward — there is nowhere to print. A
//! startup failure goes to a message box, because a daemon that exits silently
//! when its config is wrong is indistinguishable from one that is running fine.
//! Everything after startup goes to the log, which the tray menu can open.

#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;

use monitor_switcher::{config::Config, log, tray};
use windows::core::{w, PCWSTR};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

fn main() -> ExitCode {
    let config = match config_path() {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    log::to_file(log::beside(&config));

    match tray::run(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

/// `--config <path>`, or the same default the CLI uses.
///
/// Deliberately not the full clap parser: the daemon takes one option, and
/// borrowing the CLI's would invite `monitor-switcher-tray list`, which would
/// print into the void.
fn config_path() -> anyhow::Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--config") => match args.next() {
            Some(p) => Ok(PathBuf::from(p)),
            None => anyhow::bail!("--config needs a path after it"),
        },
        Some(other) => anyhow::bail!(
            "monitor-switcher-tray takes only --config <path>, not \"{other}\".\n\
             It runs the hotkeys from the config; the commands themselves are \
             monitor-switcher's."
        ),
        None => Config::default_path(),
    }
}

/// Report a startup failure the only way a windowless process can.
fn fail(e: &anyhow::Error) -> ExitCode {
    let mut text = format!("{e}");
    for cause in e.chain().skip(1) {
        text.push_str(&format!("\n\ncaused by: {cause}"));
    }
    log!("{text}");

    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    // SAFETY: both strings outlive the call.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(wide.as_ptr()),
            w!("monitor-switcher-tray"),
            MB_OK | MB_ICONERROR,
        );
    }
    ExitCode::FAILURE
}
