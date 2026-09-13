//! Config file: named targets, named profiles, and the switch pair.
//!
//! Lives at `%LOCALAPPDATA%\monitor-switcher\config.json` unless `--config`
//! says otherwise. Written by `save-profile`, hand-editable afterwards.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
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
    /// HDMI-CEC power control for this display, if it has any. Absent means
    /// the display is only ever switched at the GPU, never powered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cec: Option<CecConfig>,
}

/// Per-display CEC behaviour. Every action is opt-out, because a display with
/// a `cec` block at all is one you want powered along with the topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CecConfig {
    /// Which numbered HDMI input on the TV the adapter's signal arrives at.
    /// Determines the physical address claimed, and so whether taking over the
    /// input works.
    #[serde(default = "default_hdmi_port")]
    pub hdmi_port: u8,
    /// Wake the display when a profile activates it.
    #[serde(default = "yes")]
    pub power_on: bool,
    /// Put the display into standby when a profile deactivates it.
    #[serde(default = "yes")]
    pub standby: bool,
    /// Take over the display's input after waking it, so it shows this PC
    /// rather than whatever else is attached to it.
    #[serde(default = "yes")]
    pub activate_source: bool,
}

fn default_hdmi_port() -> u8 {
    1
}

fn yes() -> bool {
    true
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
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "does not exist".to_string(),
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
        Ok(PathBuf::from(base)
            .join("monitor-switcher")
            .join("config.json"))
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

    /// Write the config out, atomically.
    ///
    /// Deliberately not a plain `write`: that truncates the file and then fills
    /// it, so an interruption in between — a crash, a full disk, a killed
    /// process — leaves a half-written config where a working one used to be.
    /// The read path would report that as a parse error, which is honest but no
    /// comfort at all when the profiles are gone. Writing a sibling file and
    /// renaming it over the target means the config on disk is only ever the
    /// whole old one or the whole new one.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }

        // Alongside the target, so the rename stays on one volume — across
        // volumes it degrades into a copy, which is the thing being avoided.
        // The pid keeps two concurrent runs off each other's temporary file.
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".{}.tmp", std::process::id()));
        let tmp = PathBuf::from(tmp);

        let result = self.write_and_rename(&tmp, path);
        if result.is_err() {
            // Don't leave a stray half-written sibling next to the real config.
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    fn write_and_rename(&self, tmp: &Path, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)? + "\n";

        // Written through an explicit handle rather than `fs::write` so that
        // flushing can be checked: a close that fails — the usual way a full
        // disk announces itself — is otherwise discarded, and the rename would
        // then publish a file that was never fully written.
        let mut file = File::create(tmp).with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(text.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("flushing {} to disk", tmp.display()))?;
        // Closed before the move: Windows is particular about renaming a file
        // that still has an open handle.
        drop(file);

        std::fs::rename(tmp, path)
            .with_context(|| format!("moving {} into place as {}", tmp.display(), path.display()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::TargetKey;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scratch directory that removes itself, so a failing test leaves no
    /// litter in the system temp directory.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "monitor-switcher-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("creating the scratch directory");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config_with(profile: &str, members: &[&str]) -> Config {
        let mut config = Config::default();
        for (i, name) in members.iter().enumerate() {
            config.targets.insert(
                (*name).to_string(),
                TargetEntry {
                    key: TargetKey {
                        adapter: "adapter-0".into(),
                        target_id: 100 + i as u32,
                    },
                    edid: None,
                    friendly: None,
                    cec: None,
                },
            );
        }
        config.profiles.insert(
            profile.to_string(),
            members.iter().map(|s| (*s).to_string()).collect(),
        );
        config
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("reading the scratch directory")
            .map(|e| {
                e.expect("a directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = TempDir::new();
        let path = tmp.path().join("config.json");
        config_with("tv", &["main", "tv"]).save(&path).unwrap();

        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.profiles["tv"], vec!["main", "tv"]);
        assert_eq!(loaded.targets["main"].key.target_id, 100);
        assert_eq!(loaded.targets["tv"].key.target_id, 101);
    }

    #[test]
    fn save_replaces_an_existing_config() {
        let tmp = TempDir::new();
        let path = tmp.path().join("config.json");
        config_with("old", &["main"]).save(&path).unwrap();
        config_with("new", &["main"]).save(&path).unwrap();

        let loaded = Config::load(&path).unwrap();
        assert!(loaded.profiles.contains_key("new"));
        assert!(
            !loaded.profiles.contains_key("old"),
            "the replaced profile survived"
        );
    }

    #[test]
    fn save_leaves_no_temporary_file_behind() {
        let tmp = TempDir::new();
        let path = tmp.path().join("config.json");
        config_with("tv", &["main"]).save(&path).unwrap();
        assert_eq!(entries(tmp.path()), ["config.json"]);
    }

    #[test]
    fn save_creates_the_directory_it_needs() {
        let tmp = TempDir::new();
        let path = tmp.path().join("nested").join("deeper").join("config.json");
        config_with("tv", &["main"]).save(&path).unwrap();
        assert!(path.is_file());
    }

    /// The whole point of the temp-file-and-rename dance: a save that cannot
    /// finish must not take the existing config down with it, and must not
    /// leave its working file lying around either.
    #[test]
    fn a_save_that_cannot_finish_reports_it_and_tidies_up() {
        let tmp = TempDir::new();
        // A non-empty directory cannot be replaced by a rename, so the move at
        // the end of `save` fails after the temporary file has been written.
        let path = tmp.path().join("config.json");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("occupied"), "x").unwrap();

        let err = config_with("tv", &["main"]).save(&path).unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains("config.json"), "{rendered}");
        assert_eq!(
            entries(tmp.path()),
            ["config.json"],
            "a temporary file was left behind"
        );
    }

    #[test]
    fn load_or_default_accepts_a_missing_file() {
        let tmp = TempDir::new();
        let config = Config::load_or_default(&tmp.path().join("absent.json")).unwrap();
        assert!(config.targets.is_empty() && config.profiles.is_empty());
    }

    #[test]
    fn load_explains_a_missing_file_rather_than_inventing_one() {
        let tmp = TempDir::new();
        let path = tmp.path().join("absent.json");
        let err = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(err.contains("absent.json"), "{err}");
        assert!(err.contains("save-profile"), "{err}");
    }

    #[test]
    fn a_corrupt_config_is_a_parse_error_not_an_empty_config() {
        let tmp = TempDir::new();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        let err = format!("{:#}", Config::load_or_default(&path).unwrap_err());
        assert!(err.contains("parsing config"), "{err}");
    }
}
