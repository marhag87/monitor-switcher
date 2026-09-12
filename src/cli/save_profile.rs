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
