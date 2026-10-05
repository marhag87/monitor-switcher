//! `hover` — Windows' "Activate on hover" setting, from the command line.

use anyhow::Result;

use crate::config::Config;
use crate::hover;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Report whether it is on, and its delay
    Status,
    /// Turn it on
    On,
    /// Turn it off
    Off,
    /// Turn it on if it is off, and off if it is on
    Switch,
    /// Show or set how long the mouse must rest before a window activates
    Delay {
        /// New delay in milliseconds; 0 is immediate. Settings stops at 100.
        ms: Option<u32>,
    },
}

pub fn run(config: &Config, command: Command) -> Result<()> {
    let was = hover::enabled()?;
    let want = match command {
        Command::Status => {
            crate::log!(
                "Activate on hover: {}, delay {} ms",
                word(was),
                hover::delay()?
            );
            return Ok(());
        }
        Command::Delay { ms } => return delay(ms),
        Command::On => true,
        Command::Off => false,
        Command::Switch => !was,
    };
    // The configured delay goes in whenever the outcome is "on" — even when it
    // already was — and before the switch is thrown, so the very first hover
    // after turning it on already behaves as configured.
    if want {
        if let Some(ms) = config.hover.as_ref().and_then(|h| h.delay_ms) {
            delay(Some(ms))?;
        }
    }
    if want == was {
        crate::log!("Activate on hover: already {}", word(was));
        // Still shown: the notice is how a hotkey press says where things
        // stand, and "already on" answers that as well as "now on".
        notice(was);
        return Ok(());
    }
    hover::set(want)?;
    // Read back rather than assumed: the call succeeding is not quite the same
    // as the setting having taken, and this is the line that says which.
    let now = hover::enabled()?;
    crate::log!("Activate on hover: {} -> {}", word(was), word(now));
    // What Windows now reports, not what was asked for — the same reading the
    // log line just used.
    notice(now);
    if now != want {
        anyhow::bail!(
            "Windows accepted the change but still reports activate-on-hover as {}",
            word(now)
        );
    }
    Ok(())
}

fn delay(ms: Option<u32>) -> Result<()> {
    let was = hover::delay()?;
    let Some(want) = ms else {
        crate::log!("Activate on hover delay: {was} ms");
        return Ok(());
    };
    if want == was {
        crate::log!("Activate on hover delay: already {was} ms");
        return Ok(());
    }
    hover::set_delay(want)?;
    let now = hover::delay()?;
    crate::log!("Activate on hover delay: {was} ms -> {now} ms");
    if now != want {
        anyhow::bail!("Windows accepted the change but still reports the delay as {now} ms");
    }
    Ok(())
}

fn notice(on: bool) {
    crate::notify::show(crate::notify::Notice {
        label: "Activate on hover".to_string(),
        on,
    });
}

fn word(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}
