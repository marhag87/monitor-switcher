//! `save-profile` — record which outputs are active right now, under a name.
//!
//! Only touches the config file; the display is left alone. Any output not yet
//! named gets registered under a generated name so you never hand-copy a target
//! id — rename it in the config afterwards if you want something shorter.
//!
//! Deliberately records *only which outputs are on*, not their resolutions or
//! positions. Windows keeps its own database of layouts and `apply-profile` asks
//! it for them, so duplicating that here would just be a second copy to go stale.

use std::path::Path;

use anyhow::{bail, Result};

use crate::config::{Config, TargetEntry};
use crate::display::Monitor;

pub fn run(
    monitors: &[Monitor],
    config: &mut Config,
    path: &Path,
    name: &str,
    force: bool,
) -> Result<()> {
    if config.profiles.contains_key(name) && !force {
        bail!("profile \"{name}\" already exists; pass --force to overwrite");
    }

    let active: Vec<&Monitor> = monitors.iter().filter(|m| m.active).collect();
    if active.is_empty() {
        bail!("no active displays found — nothing to save");
    }

    let mut members = Vec::new();
    let mut newly_named = Vec::new();

    for m in &active {
        let target_name = match config.name_for(&m.key) {
            Some(existing) => existing.to_string(),
            None => {
                let generated = unique_name(config, m);
                config.targets.insert(
                    generated.clone(),
                    TargetEntry {
                        key: m.key.clone(),
                        edid: m.name.edid.clone(),
                        friendly: m.name.friendly.clone(),
                        // Power control is opt-in: a newly discovered display
                        // gets switched at the GPU only, until you say otherwise.
                        cec: None,
                    },
                );
                newly_named.push(generated.clone());
                generated
            }
        };
        members.push(target_name);
    }

    config.profiles.insert(name.to_string(), members.clone());
    config.save(path)?;

    println!("Saved profile \"{name}\": {}", members.join(", "));
    if !newly_named.is_empty() {
        println!(
            "Registered {} new target(s): {}",
            newly_named.len(),
            newly_named.join(", ")
        );
        println!(
            "Rename them in {} if you'd like shorter names.",
            path.display()
        );
    }
    if config.switch.is_none() && config.profiles.len() >= 2 {
        let names: Vec<&str> = config.profiles.keys().map(|s| s.as_str()).collect();
        println!(
            "\nTo make bare `switch` work, add to {}:\n  \"switch\": [\"{}\", \"{}\"]",
            path.display(),
            names[0],
            names[1]
        );
    }
    Ok(())
}

/// Derive a config name for a newly seen output, avoiding collisions.
fn unique_name(config: &Config, m: &Monitor) -> String {
    let base = m
        .name
        .friendly
        .as_deref()
        .map(slug)
        .filter(|s| !s.is_empty())
        .or_else(|| m.name.edid.as_deref().map(slug))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("target-{}", m.key.target_id));

    if !config.targets.contains_key(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !config.targets.contains_key(c))
        .expect("an unused suffix always exists")
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::ccd::TargetName;
    use crate::display::TargetKey;
    use windows::Win32::Foundation::LUID;

    fn monitor(friendly: Option<&str>, edid: Option<&str>, target_id: u32) -> Monitor {
        Monitor {
            key: TargetKey {
                adapter: "adapter-0".into(),
                target_id,
            },
            adapter_luid: LUID {
                LowPart: 0,
                HighPart: 0,
            },
            source_id: 0,
            active: true,
            available: true,
            name: TargetName {
                friendly: friendly.map(String::from),
                device_path: None,
                edid: edid.map(String::from),
                output_technology: 0,
            },
            mode: None,
            refresh_hz: None,
            gdi_name: None,
        }
    }

    #[test]
    fn slug_lowercases_and_joins_on_punctuation() {
        assert_eq!(slug("MPG321UX OLED"), "mpg321ux-oled");
        assert_eq!(slug("LG ULTRAGEAR"), "lg-ultragear");
        assert_eq!(slug("DELL U2415"), "dell-u2415");
    }

    #[test]
    fn slug_collapses_runs_and_trims_the_ends() {
        assert_eq!(slug("  A  B  "), "a-b");
        assert_eq!(slug("a---b"), "a-b");
        assert_eq!(slug("...x..."), "x");
    }

    #[test]
    fn slug_of_nothing_usable_is_empty() {
        assert_eq!(slug(""), "");
        assert_eq!(slug("!!!"), "");
    }

    #[test]
    fn unique_name_prefers_the_friendly_name() {
        let config = Config::default();
        let m = monitor(Some("MPG321UX OLED"), Some("MSI3DD2"), 37121);
        assert_eq!(unique_name(&config, &m), "mpg321ux-oled");
    }

    #[test]
    fn unique_name_falls_back_to_the_edid_id() {
        let config = Config::default();
        let m = monitor(None, Some("MSI3DD2"), 37121);
        assert_eq!(unique_name(&config, &m), "msi3dd2");
    }

    /// A friendly name of nothing but punctuation slugs to an empty string,
    /// which must not become the target's name.
    #[test]
    fn unique_name_skips_a_friendly_name_that_slugs_to_nothing() {
        let config = Config::default();
        let m = monitor(Some("!!!"), Some("MSI3DD2"), 37121);
        assert_eq!(unique_name(&config, &m), "msi3dd2");
    }

    #[test]
    fn unique_name_falls_back_to_the_target_id() {
        let config = Config::default();
        let m = monitor(None, None, 37121);
        assert_eq!(unique_name(&config, &m), "target-37121");
    }

    /// Two identical panels are ordinary — the suffix is what keeps the second
    /// one from overwriting the first.
    #[test]
    fn unique_name_suffixes_around_a_collision() {
        let mut config = Config::default();
        let m = monitor(Some("DELL U2415"), None, 37123);

        for expected in ["dell-u2415", "dell-u2415-2", "dell-u2415-3"] {
            let name = unique_name(&config, &m);
            assert_eq!(name, expected);
            config.targets.insert(
                name,
                TargetEntry {
                    key: m.key.clone(),
                    edid: None,
                    friendly: None,
                    cec: None,
                },
            );
        }
    }
}
