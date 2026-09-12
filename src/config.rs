//! Config file: named targets, named profiles, and the switch pair.
//!
//! Lives at `%LOCALAPPDATA%\monitor-switcher\config.json` unless `--config`
//! says otherwise. Written by `save-profile`, hand-editable afterwards.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::display::TargetKey;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Friendly name -> the physical output it refers to.
    #[serde(default)]
    pub targets: BTreeMap<String, TargetEntry>,
    /// Profile name -> the target names that should be active.
    #[serde(default)]
    pub profiles: BTreeMap<String, Vec<String>>,
    /// The two profiles `switch` alternates between when given no arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetEntry {
    #[serde(flatten)]
    pub key: TargetKey,
    /// EDID id, e.g. `MSI3DD2`. Not used for topology — recorded because it is
    /// how DDC/CI addresses a panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edid: Option<String>,
    /// Whatever the monitor called itself when captured. Comment, not key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friendly: Option<String>,
}

/// Read and parse the config. `Ok(None)` means the file genuinely is not there.
///
/// Deliberately not written as `if path.exists()` followed by a read: `exists()`
/// reports `false` for *every* failure to stat the file, so a permission denial
/// or a transient lock from an antivirus scanner is indistinguishable from a
/// missing file. That turns a real problem into "run save-profile first", which
/// sends you looking in the wrong place. Read it and let the error say what
/// actually happened.
fn read(path: &Path) -> Result<Option<Config>> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .with_context(|| format!("parsing config at {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading config at {}", path.display())),
    }
}

/// Explain a missing config in enough detail to tell apart the three reasons it
/// happens: never created, created somewhere else because `LOCALAPPDATA` differs
/// between shells, or present but invisible to this process.
fn missing_config_message(path: &Path) -> String {
    let mut msg = format!("no config at {}", path.display());

    match path.parent() {
        Some(dir) if dir.as_os_str().is_empty() => {}
        Some(dir) => {
            let state = match std::fs::read_dir(dir) {
                Ok(entries) => {
                    let names: Vec<String> = entries
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect();
                    if names.is_empty() {
                        "exists but is empty".to_string()
                    } else {
                        format!("exists and contains: {}", names.join(", "))
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    "does not exist".to_string()
                }
                Err(e) => format!("cannot be read: {e}"),
            };
            msg.push_str(&format!("\n  the directory {} {}", dir.display(), state));
        }
        None => {}
    }

    if let Ok(v) = std::env::var("LOCALAPPDATA") {
        msg.push_str(&format!("\n  LOCALAPPDATA for this process is {v}"));
    } else {
        msg.push_str("\n  LOCALAPPDATA is not set for this process");
    }

    msg.push_str(
        "\n  Run `monitor-switcher save-profile <name>` to create one, \
         or pass --config <path> to use one elsewhere.",
    );
    msg
}

impl Config {
    pub fn default_path() -> Result<PathBuf> {
        let base = std::env::var("LOCALAPPDATA")
            .context("LOCALAPPDATA is not set; pass --config with an explicit path")?;
        Ok(PathBuf::from(base).join("monitor-switcher").join("config.json"))
    }

    /// Load the config, or an empty one if the file does not exist yet.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        match read(path)? {
            Some(cfg) => Ok(cfg),
            None => Ok(Self::default()),
        }
    }

    /// Load the config, failing if it does not exist — for commands that cannot
    /// do anything useful without one.
    pub fn load(path: &Path) -> Result<Self> {
        read(path)?.ok_or_else(|| anyhow::anyhow!(missing_config_message(path)))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text + "\n")
            .with_context(|| format!("writing config to {}", path.display()))
    }

    /// Look up the name given to a physical output, if it has one.
    pub fn name_for(&self, key: &TargetKey) -> Option<&str> {
        self.targets
            .iter()
            .find(|(_, e)| &e.key == key)
            .map(|(n, _)| n.as_str())
    }

    /// Resolve the two profiles `switch` should alternate between.
    pub fn switch_pair(&self) -> Result<(String, String)> {
        let pair = self.switch.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no switch pair configured\n  \
                 Add `\"switch\": [\"<profile-a>\", \"<profile-b>\"]` to the config, \
                 or pass both names: `monitor-switcher switch <a> <b>`"
            )
        })?;
        match pair.as_slice() {
            [a, b] => Ok((a.clone(), b.clone())),
            other => bail!(
                "\"switch\" must name exactly 2 profiles, found {}",
                other.len()
            ),
        }
    }
}
