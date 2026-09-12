//! `vcp` — read and write a monitor's own settings over DDC/CI.
//!
//! Shaped to match ControlMyMonitor, which this replaces, including its
//! convention that **VCP codes are hex and values are decimal**. So
//! `ControlMyMonitor.exe /SwitchValue MSI3DD2 60 15 18` becomes
//! `monitor-switcher vcp switch main 60 15 18` — code `0x60` (input select),
//! alternating between DisplayPort-1 (15) and HDMI-2 (18).

use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::ddc::Ddc;
use crate::display;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Read a feature's current value
    Get {
        target: String,
        /// VCP code, in hex (e.g. 60 for input select, 10 for brightness)
        code: String,
    },
    /// Set a feature to a value
    Set {
        target: String,
        /// VCP code, in hex
        code: String,
        /// New value, in decimal
        value: u32,
    },
    /// Step a feature to the next of the given values, wrapping around
    Switch {
        target: String,
        /// VCP code, in hex
        code: String,
        /// Values to cycle through, in decimal
        #[arg(required = true, num_args = 2..)]
        values: Vec<u32>,
    },
    /// Show every feature the display admits to supporting
    Caps { target: String },
}

pub fn run(config: &Config, command: Command) -> Result<()> {
    match command {
        Command::Get { target, code } => {
            let code = parse_code(&code)?;
            let vcp = open(config, &target)?.get(code)?;
            println!(
                "{target}: VCP 0x{code:02X} = {} (max {})",
                vcp.current, vcp.maximum
            );
        }
        Command::Set {
            target,
            code,
            value,
        } => {
            let code = parse_code(&code)?;
            open(config, &target)?.set(code, value)?;
            println!("{target}: VCP 0x{code:02X} set to {value}");
        }
        Command::Switch {
            target,
            code,
            values,
        } => {
            let code = parse_code(&code)?;
            let ddc = open(config, &target)?;
            let current = ddc.get(code)?.current;
            // Step to the value after the current one. If the display is on
            // something not in the list, the first entry is the sensible
            // destination rather than an error.
            let next = match values.iter().position(|&v| v == current) {
                Some(i) => values[(i + 1) % values.len()],
                None => values[0],
            };
            ddc.set(code, next)?;
            println!("{target}: VCP 0x{code:02X} {current} -> {next}");
        }
        Command::Caps { target } => {
            println!("{}", open(config, &target)?.capabilities()?);
        }
    }
    Ok(())
}

/// VCP codes are conventionally written in hex without a prefix, the way both
/// the MCCS standard and ControlMyMonitor present them. `0x60` is accepted too.
fn parse_code(text: &str) -> Result<u8> {
    let cleaned = text.trim_start_matches("0x").trim_start_matches("0X");
    u8::from_str_radix(cleaned, 16)
        .with_context(|| format!("\"{text}\" is not a VCP code; expected hex, e.g. 60 or 0x60"))
}

/// Resolve a configured target name — or an EDID id — to an open DDC/CI channel.
fn open(config: &Config, target: &str) -> Result<Ddc> {
    let monitors = display::enumerate()?;

    // Prefer a configured name; fall back to EDID id, since that is how
    // ControlMyMonitor addresses displays and what its old commands contain.
    let wanted = config.targets.get(target).map(|e| &e.key);
    let monitor = monitors
        .iter()
        .find(|m| match wanted {
            Some(key) => &m.key == key,
            None => m.name.edid.as_deref() == Some(target),
        })
        .ok_or_else(|| {
            let known: Vec<&str> = config.targets.keys().map(|s| s.as_str()).collect();
            anyhow::anyhow!(
                "no display named \"{target}\"; configured targets: {}",
                known.join(", ")
            )
        })?;

    // DDC/CI rides on the video link, so there is nothing to talk to when the
    // output is switched off. Say that plainly — it is a likely mistake when
    // one of a pair of swing displays is the inactive one.
    let Some(gdi) = monitor.gdi_name.as_deref() else {
        bail!(
            "\"{target}\" is not currently active, so it has no DDC/CI channel.\n  \
             Switch to a profile that includes it first."
        );
    };
    Ddc::open(gdi)
}
