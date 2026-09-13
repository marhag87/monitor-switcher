//! monitor-switcher — swap which display outputs are active, reliably.
//!
//! Built around the CCD API (`QueryDisplayConfig` / `SetDisplayConfig`) rather
//! than the legacy GDI display API, because only CCD can see — and therefore
//! re-enable — an output that is currently switched off.
//!
//! This is a library only so that two binaries can share it: the console
//! `monitor-switcher`, and the GUI-subsystem `monitor-switcher-tray` that sits
//! in the notification area and runs the same commands from a hotkey. They need
//! opposite Windows subsystems — one has to be able to print, the other must not
//! own a console — and that is a property of a binary, not of the code.

pub mod app;
#[cfg(feature = "cec")]
pub mod cec;
pub mod cli;
pub mod config;
pub mod ddc;
pub mod display;
pub mod hotkey;
pub mod icon;
#[macro_use]
pub mod log;
pub mod tray;
pub mod winerr;
