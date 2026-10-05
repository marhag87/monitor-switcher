//! "Activate on hover" — focus follows the mouse.
//!
//! Settings files this under Accessibility > Mouse, but underneath it is the
//! decades-old *active window tracking* (the "X-Mouse" setting), read and
//! written with `SystemParametersInfo`. It is per-user and needs no elevation.
//!
//! The switch and the hover delay beneath it are separate parameters, so
//! changing one leaves the other alone, exactly as in Settings. "Move window to
//! top" is a third, and is not touched at all.

use std::ffi::c_void;

use anyhow::{Context, Result};
use windows::core::BOOL;
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_GETACTIVEWINDOWTRACKING,
    SPI_GETACTIVEWNDTRKTIMEOUT, SPI_SETACTIVEWINDOWTRACKING, SPI_SETACTIVEWNDTRKTIMEOUT,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

/// Whether activate-on-hover is currently on.
pub fn enabled() -> Result<bool> {
    let mut on = BOOL(0);
    // SAFETY: the GET form writes one BOOL through pvParam, which points at `on`.
    unsafe {
        SystemParametersInfoW(
            SPI_GETACTIVEWINDOWTRACKING,
            0,
            Some(&mut on as *mut BOOL as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .context("reading the activate-on-hover setting")?;
    Ok(on.as_bool())
}

/// Turn activate-on-hover on or off, for this session and every later one.
pub fn set(on: bool) -> Result<()> {
    // SAFETY: the SET form takes the new value *as* pvParam rather than through
    // it — the pointer is never dereferenced, so any integer is sound.
    unsafe {
        SystemParametersInfoW(
            SPI_SETACTIVEWINDOWTRACKING,
            0,
            Some(on as usize as *mut c_void),
            // UPDATEINIFILE persists it to the profile, so it survives signing
            // out; without it the change lasts until then and Settings, which
            // reads the stored value, would disagree with what the mouse does.
            // SENDCHANGE tells running programs, Settings among them.
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        )
    }
    .context("changing the activate-on-hover setting")
}

/// How long, in milliseconds, the mouse must rest over a window before it is
/// activated.
pub fn delay() -> Result<u32> {
    let mut ms = 0u32;
    // SAFETY: the GET form writes one DWORD through pvParam, which points at `ms`.
    unsafe {
        SystemParametersInfoW(
            SPI_GETACTIVEWNDTRKTIMEOUT,
            0,
            Some(&mut ms as *mut u32 as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .context("reading the activate-on-hover delay")?;
    Ok(ms)
}

/// Set the hover delay, for this session and every later one.
///
/// This is the reason the setting is worth reaching past Settings for: its
/// slider stops at 100 ms, which is a perceptible lag, while Windows itself
/// accepts anything down to 0.
pub fn set_delay(ms: u32) -> Result<()> {
    // SAFETY: as with `set`, the value travels as pvParam itself and is never
    // dereferenced.
    unsafe {
        SystemParametersInfoW(
            SPI_SETACTIVEWNDTRKTIMEOUT,
            0,
            Some(ms as usize as *mut c_void),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        )
    }
    .context("changing the activate-on-hover delay")
}
