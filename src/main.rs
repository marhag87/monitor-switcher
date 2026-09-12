//! monitor-switcher — swap which display outputs are active, reliably.
//!
//! Built around the CCD API (`QueryDisplayConfig` / `SetDisplayConfig`) rather
//! than the legacy GDI display API, because only CCD can see — and therefore
//! re-enable — an output that is currently switched off.

#[cfg(feature = "cec")]
mod cec;
mod cli;
mod config;
mod ddc;
mod display;
mod winerr;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use config::Config;

#[derive(Parser)]
#[command(
    name = "monitor-switcher",
    about = "Switch display topology via the Windows CCD API",
    version
)]
struct Cli {
    /// Config file location (default: %LOCALAPPDATA%\monitor-switcher\config.json)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List every connected output and its stable identity
    List {
        /// Only show outputs that are currently active
        #[arg(long)]
        active: bool,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Record the currently active outputs as a named profile
    SaveProfile {
        /// Name for the profile
        name: String,
        /// Overwrite an existing profile of this name
        #[arg(long)]
        force: bool,
    },
    /// Make a named profile's outputs the active ones
    ApplyProfile {
        /// Profile to apply
        name: String,
        #[command(flatten)]
        dry: DryRun,
    },
    /// Power a display on or off over HDMI-CEC, without changing topology
    #[cfg(feature = "cec")]
    Cec {
        /// What to do
        #[arg(value_enum)]
        action: cli::cec::Action,
        /// Which target, if more than one supports CEC
        target: Option<String>,
    },
    /// Read or change a monitor's own settings over DDC/CI
    Vcp {
        #[command(subcommand)]
        command: cli::vcp::Command,
    },
    /// Alternate between two profiles
    Switch {
        /// The two profiles to alternate between (defaults to the config's pair)
        #[arg(num_args = 0..=2)]
        profiles: Vec<String>,
        #[command(flatten)]
        dry: DryRun,
    },
}

#[derive(Args)]
struct DryRun {
    /// Validate the change without applying it
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Print the whole context chain: a silent or vague failure is the
            // problem this tool was written to avoid.
            eprintln!("error: {e}");
            for cause in e.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let config_path = match cli.config {
        Some(p) => p,
        None => Config::default_path()?,
    };

    match cli.command {
        Command::List { active, json } => {
            let monitors = display::enumerate()?;
            let config = Config::load_or_default(&config_path)?;
            cli::list::run(&monitors, &config, active, json)
        }
        Command::SaveProfile { name, force } => {
            let monitors = display::enumerate()?;
            let mut config = Config::load_or_default(&config_path)?;
            cli::save_profile::run(&monitors, &mut config, &config_path, &name, force)
        }
        Command::ApplyProfile { name, dry } => {
            let config = Config::load(&config_path)?;
            let report_ = display::apply::apply_profile(&config, &name, dry.dry_run)?;
            report(&name, report_);
            Ok(())
        }
        #[cfg(feature = "cec")]
        Command::Cec { action, target } => {
            let config = Config::load(&config_path)?;
            cli::cec::run(&config, action, target.as_deref())
        }
        Command::Vcp { command } => {
            let config = Config::load_or_default(&config_path)?;
            cli::vcp::run(&config, command)
        }
        Command::Switch { profiles, dry } => {
            let config = Config::load(&config_path)?;
            let (a, b) = match profiles.as_slice() {
                [] => config.switch_pair()?,
                [a, b] => (a.clone(), b.clone()),
                _ => anyhow::bail!("switch takes either no profiles or exactly 2"),
            };
            let (applied, report_) = display::apply::switch(&config, &a, &b, dry.dry_run)?;
            report(&applied, report_);
            Ok(())
        }
    }
}

fn report(profile: &str, report: display::apply::Report) {
    use display::apply::Outcome;
    match report.outcome {
        Outcome::AlreadyActive => println!("\"{profile}\" is already active"),
        Outcome::Validated => println!("\"{profile}\" validates; nothing was changed"),
        Outcome::Applied(tier) => println!("Applied \"{profile}\" — {}", tier.describe()),
    }
    for note in report.cec {
        println!("  {note}");
    }
}
