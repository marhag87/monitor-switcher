//! Short on-screen notices, for changes you cannot otherwise see happen.
//!
//! Toggling "Activate on hover" from a hotkey changes nothing on screen, so
//! without a notice you find out which way it went by moving the mouse and
//! seeing what follows it. Commands call [`show`]; whether anything appears
//! depends on who is running them.
//!
//! Only the tray daemon installs a sink, showing the notice as a
//! [flyout](crate::flyout). On the command line there is no sink and `show`
//! does nothing, because the command has already printed the same thing to the
//! console you are looking at.

use std::sync::OnceLock;

/// A setting that was just switched, and where it ended up.
#[derive(Debug, Clone)]
pub struct Notice {
    pub label: String,
    pub on: bool,
}

type Sink = Box<dyn Fn(Notice) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Route notices somewhere. Called once, by the daemon, at startup.
pub fn set_sink(sink: impl Fn(Notice) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

/// Show a notice, if anything is listening. Never fails: a notice that cannot
/// be shown is not worth failing the command over, and the log still has it.
pub fn show(notice: Notice) {
    if let Some(sink) = SINK.get() {
        sink(notice);
    }
}
