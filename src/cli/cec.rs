//! `cec` — drive a display's power directly, without touching topology.
//!
//! Useful on its own, and the quickest way to tell whether the adapter is
//! working when a `switch` doesn't do what you expected.

use anyhow::{bail, Result};

use crate::cec::Cec;
use crate::config::{CecConfig, Config};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Action {
    /// Report the display's power state
    Status,
    /// Wake the display and take over its input
    On,
    /// Put the display into standby
    Off,
}

pub fn run(config: &Config, action: Action, target: Option<&str>) -> Result<()> {
    let (name, conf) = pick(config, target)?;
    let cec = Cec::open(conf.hdmi_port)?;

    match action {
        Action::Status => println!("{name}: {:?}", cec.power_status()),
        Action::On => println!("{name}: {}", cec.wake(conf.activate_source)?),
        Action::Off => println!("{name}: {}", cec.sleep()?),
    }
    Ok(())
}

/// Find the target to act on: the named one, or the only CEC-capable one.
fn pick<'a>(config: &'a Config, target: Option<&str>) -> Result<(&'a str, &'a CecConfig)> {
    let capable: Vec<(&str, &CecConfig)> = config
        .targets
        .iter()
        .filter_map(|(n, e)| e.cec.as_ref().map(|c| (n.as_str(), c)))
        .collect();

    match target {
        Some(want) => capable
            .into_iter()
            .find(|(n, _)| *n == want)
            .ok_or_else(|| anyhow::anyhow!("target \"{want}\" has no \"cec\" block in the config")),
        None => match capable.as_slice() {
            [one] => Ok(*one),
            [] => bail!("no target in the config has a \"cec\" block"),
            many => bail!(
                "several targets support CEC ({}); name the one you mean",
                many.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
            ),
        },
    }
}
