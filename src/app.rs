//! The command line: one definition, used by both binaries.
//!
//! The tray daemon runs a hotkey's action by handing this the same words you
//! would have typed, so anything the CLI can do a hotkey can do — and a typo in
//! the config is caught by the same parser that would have caught it at a
//! prompt, rather than by something written a second time and drifting.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::cli;
use crate::config::Config;
use crate::display;

#[derive(Parser)]
#[command(
    name = "monitor-switcher",
    about = "Switch display topology via the Windows CCD API",
    version
)]
pub struct Cli {
    /// Config file location (default: %LOCALAPPDATA%\monitor-switcher\config.json)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
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
pub struct DryRun {
    /// Validate the change without applying it
    #[arg(long)]
    pub dry_run: bool,
}

/// The console binary's entry point.
pub fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => match run(cli) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                report_error(&e);
                ExitCode::FAILURE
            }
        },
        // clap already renders --help and --version as errors that want
        // printing and a success exit; anything else is a usage problem.
        Err(e) => {
            let _ = e.print();
            if e.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}

/// Print a failure the way this tool insists on: the whole context chain, since
/// a silent or vague failure is the problem it was written to avoid.
pub fn report_error(e: &anyhow::Error) {
    eprintln!("error: {e}");
    for cause in e.chain().skip(1) {
        eprintln!("  caused by: {cause}");
    }
}

/// Run one already-parsed invocation.
pub fn run(cli: Cli) -> Result<()> {
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

/// Run a command written the way it would be typed, against a known config.
///
/// This is how the daemon executes a hotkey's action. The config path is passed
/// ahead of the caller's words rather than appended, so a command that names its
/// own `--config` still wins — clap takes the last occurrence.
pub fn run_words(words: &[String], config: &Path) -> Result<()> {
    let argv = ["monitor-switcher".to_string(), "--config".to_string()]
        .into_iter()
        .chain(std::iter::once(config.to_string_lossy().into_owned()))
        .chain(words.iter().cloned());
    run(Cli::try_parse_from(argv)?)
}

/// Check that a command would parse, without running it.
///
/// Used at daemon start so a typo in a configured hotkey is reported then,
/// rather than becoming a key that silently does nothing.
pub fn check_words(words: &[String]) -> Result<()> {
    let argv = std::iter::once("monitor-switcher".to_string()).chain(words.iter().cloned());
    Cli::try_parse_from(argv)?;
    Ok(())
}

/// Split a configured action into words, the way a shell would.
///
/// Only double quotes are honoured, which covers the one case that actually
/// turns up — a target or profile whose name has a space in it — without
/// pretending to be a shell.
pub fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;

    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                // An empty pair of quotes is still an argument.
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

fn report(profile: &str, report: display::apply::Report) {
    use display::apply::Outcome;
    match report.outcome {
        Outcome::AlreadyActive => crate::log!("\"{profile}\" is already active"),
        Outcome::Validated => crate::log!("\"{profile}\" validates; nothing was changed"),
        Outcome::Applied(tier) => crate::log!("Applied \"{profile}\" — {}", tier.describe()),
    }
    for note in report.cec {
        crate::log!("  {note}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        split_words(line)
    }

    /// The two actions actually bound on this desk, straight out of the
    /// shortcuts they replace.
    #[test]
    fn split_words_handles_the_real_actions() {
        assert_eq!(words("switch"), ["switch"]);
        assert_eq!(
            words("vcp switch main 60 15 18"),
            ["vcp", "switch", "main", "60", "15", "18"]
        );
    }

    #[test]
    fn split_words_ignores_surplus_whitespace() {
        assert_eq!(words("  switch   tv  "), ["switch", "tv"]);
        assert_eq!(words(""), Vec::<String>::new());
        assert_eq!(words("   "), Vec::<String>::new());
    }

    #[test]
    fn split_words_keeps_a_quoted_argument_together() {
        assert_eq!(
            words(r#"apply-profile "living room""#),
            ["apply-profile", "living room"]
        );
        assert_eq!(words(r#""" x"#), ["", "x"]);
    }

    /// Every configured action is parsed by the CLI's own parser, so a typo is
    /// a startup error rather than a hotkey that does nothing.
    #[test]
    fn a_configured_action_is_checked_against_the_real_parser() {
        let ok = Cli::try_parse_from(["monitor-switcher", "switch"]);
        assert!(ok.is_ok());

        let bad = Cli::try_parse_from(["monitor-switcher", "swtich"]);
        assert!(bad.is_err(), "a misspelled subcommand was accepted");
    }

    #[test]
    fn vcp_switch_parses_with_its_values() {
        let cli = Cli::try_parse_from(words("vcp switch main 60 15 18").iter().fold(
            vec!["monitor-switcher".to_string()],
            |mut v, w| {
                v.push(w.clone());
                v
            },
        ))
        .expect("the action bound to ctrl+shift+s");
        match cli.command {
            Command::Vcp {
                command: cli::vcp::Command::Switch { target, values, .. },
            } => {
                assert_eq!(target, "main");
                assert_eq!(values, [15, 18]);
            }
            _ => panic!("parsed as the wrong command"),
        }
    }
}
