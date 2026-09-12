//! Applying a profile: making exactly the named outputs active.
//!
//! The interesting decision here is what we *don't* send. `SetDisplayConfig`
//! can take a full mode description — every resolution, position and video
//! timing — but it can also take bare paths plus `SDC_TOPOLOGY_SUPPLIED`, which
//! tells it to look the modes up in Windows' own persistence database. We do the
//! latter: Windows already remembers how each of these layouts was arranged, so
//! storing a second copy would only give us something to keep in sync.

use anyhow::{bail, Result};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_PATH_INFO, QDC_ALL_PATHS, SDC_ALLOW_CHANGES, SDC_ALLOW_PATH_ORDER_CHANGES,
    SDC_APPLY, SDC_SAVE_TO_DATABASE, SDC_TOPOLOGY_SUPPLIED, SDC_USE_SUPPLIED_DISPLAY_CONFIG,
    SDC_VALIDATE,
};
use windows::Win32::Graphics::Gdi::{
    DISPLAYCONFIG_PATH_ACTIVE, DISPLAYCONFIG_PATH_MODE_IDX_INVALID,
    DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE,
};

use super::ccd::{self, AdapterPaths};
use super::identity::{self, TargetKey};
use crate::config::Config;
use crate::winerr;

const ERROR_SUCCESS: u32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Windows supplied the modes from its persistence database. The normal,
    /// expected path.
    Database,
    /// The database had nothing for this combination, so Windows chose modes
    /// itself. Works, but the layout may not be what you arranged.
    BestMode,
}

impl Tier {
    pub fn describe(self) -> &'static str {
        match self {
            Tier::Database => "using the saved layout from Windows' display database",
            Tier::BestMode => {
                "Windows had no saved layout for this combination and picked modes itself"
            }
        }
    }
}

#[derive(Debug)]
pub enum Outcome {
    /// The requested outputs were already the active ones. No call was made.
    AlreadyActive,
    /// Validated only; nothing was changed.
    Validated,
    Applied(Tier),
}

/// Which profile, if any, describes the currently active set of outputs.
pub fn current_profile(config: &Config) -> Result<Option<String>> {
    let active = active_keys()?;
    for (name, members) in &config.profiles {
        let keys = resolve(config, members)?;
        if same_set(&keys, &active) {
            return Ok(Some(name.clone()));
        }
    }
    Ok(None)
}

fn active_keys() -> Result<Vec<TargetKey>> {
    Ok(super::enumerate()?
        .into_iter()
        .filter(|m| m.active)
        .map(|m| m.key)
        .collect())
}

fn same_set(a: &[TargetKey], b: &[TargetKey]) -> bool {
    a.len() == b.len() && a.iter().all(|k| b.contains(k))
}

/// Resolve profile member names to the outputs they refer to.
fn resolve(config: &Config, members: &[String]) -> Result<Vec<TargetKey>> {
    members
        .iter()
        .map(|name| {
            config
                .targets
                .get(name)
                .map(|e| e.key.clone())
                .ok_or_else(|| {
                    let known: Vec<&str> = config.targets.keys().map(|s| s.as_str()).collect();
                    anyhow::anyhow!(
                        "profile refers to unknown target \"{name}\"; known targets: {}",
                        known.join(", ")
                    )
                })
        })
        .collect()
}

pub fn apply_profile(config: &Config, profile: &str, dry_run: bool) -> Result<Outcome> {
    let members = config.profiles.get(profile).ok_or_else(|| {
        let known: Vec<&str> = config.profiles.keys().map(|s| s.as_str()).collect();
        anyhow::anyhow!(
            "no profile named \"{profile}\"; known profiles: {}",
            known.join(", ")
        )
    })?;
    let desired = resolve(config, members)?;
    apply_targets(&desired, dry_run)
}

fn apply_targets(desired: &[TargetKey], dry_run: bool) -> Result<Outcome> {
    let topo = ccd::query(QDC_ALL_PATHS)?;
    let monitors = identity::enumerate(&topo)?;

    let active: Vec<TargetKey> = monitors
        .iter()
        .filter(|m| m.active)
        .map(|m| m.key.clone())
        .collect();
    if !dry_run && same_set(desired, &active) {
        return Ok(Outcome::AlreadyActive);
    }

    let paths = build_paths(&topo, desired)?;

    // Validate first either way — a dry run stops here, and a real apply gets a
    // clearer error from the validate call than from a half-applied change.
    let validate_flags = SDC_VALIDATE | SDC_TOPOLOGY_SUPPLIED | SDC_ALLOW_PATH_ORDER_CHANGES;
    let rc = ccd::set(&paths, None, validate_flags);
    if dry_run {
        return if rc == ERROR_SUCCESS {
            Ok(Outcome::Validated)
        } else {
            // Validation failing under TOPOLOGY_SUPPLIED has two quite different
            // causes — no database entry (which a real apply recovers from by
            // letting Windows pick modes) and a topology the hardware cannot
            // drive (which it does not). Don't promise the recovery.
            bail!(
                "validation failed: {}\n  \
                 If the cause is a missing saved layout, a real apply would still \
                 succeed by letting Windows choose the modes; if the hardware cannot \
                 drive this combination, it would not.",
                winerr::describe(rc)
            )
        };
    }

    // Tier 1: Windows supplies the modes from its database.
    let rc = ccd::set(
        &paths,
        None,
        SDC_APPLY | SDC_TOPOLOGY_SUPPLIED | SDC_ALLOW_PATH_ORDER_CHANGES,
    );
    if rc == ERROR_SUCCESS {
        return Ok(Outcome::Applied(Tier::Database));
    }
    let tier1_err = rc;

    // Tier 2: no database entry — let Windows work out modes, and save the
    // result so tier 1 can serve the next call.
    //
    // Note these are disjoint flag sets, not additive: SDC_TOPOLOGY_SUPPLIED and
    // SDC_USE_SUPPLIED_DISPLAY_CONFIG cannot be combined, and
    // SDC_ALLOW_PATH_ORDER_CHANGES is only legal with the former.
    let rc = ccd::set(
        &paths,
        None,
        SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES | SDC_SAVE_TO_DATABASE,
    );
    if rc == ERROR_SUCCESS {
        return Ok(Outcome::Applied(Tier::BestMode));
    }

    bail!(
        "could not apply this topology.\n  \
         from the display database: {}\n  \
         letting Windows choose modes: {}",
        winerr::describe(tier1_err),
        winerr::describe(rc)
    )
}

/// Build the path array describing the desired topology.
///
/// Every path is marked active with no mode indices; `SetDisplayConfig`
/// exclusively enables what it is given, so anything absent from this array is
/// switched off as a consequence — there is no need to mark outputs inactive.
fn build_paths(
    topo: &ccd::Topology,
    desired: &[TargetKey],
) -> Result<Vec<DISPLAYCONFIG_PATH_INFO>> {
    let mut adapters = AdapterPaths::default();

    // Candidate paths per desired target. QDC_ALL_PATHS lists roughly every
    // source x target pairing, so each target has several, differing in which
    // GPU source feeds it.
    let mut candidates: Vec<Vec<usize>> = vec![Vec::new(); desired.len()];
    for (i, path) in topo.paths.iter().enumerate() {
        if !path.targetInfo.targetAvailable.as_bool() {
            continue;
        }
        let key = TargetKey {
            adapter: adapters.get(path.targetInfo.adapterId)?,
            target_id: path.targetInfo.id,
        };
        if let Some(slot) = desired.iter().position(|d| *d == key) {
            candidates[slot].push(i);
        }
    }

    for (slot, cands) in candidates.iter().enumerate() {
        if cands.is_empty() {
            let key = &desired[slot];
            let present: Vec<String> = identity::enumerate(topo)?
                .iter()
                .map(|m| format!("{} (target {})", m.label(), m.key.target_id))
                .collect();
            bail!(
                "target {} on adapter {} is not connected.\n  Currently connected: {}",
                key.target_id,
                key.adapter,
                present.join(", ")
            );
        }
    }

    // Each active path needs its own GPU source. Keep whatever source an
    // already-active output is using, then fill the rest from what's left, so a
    // swap disturbs the displays that aren't changing as little as possible.
    let mut used: Vec<u32> = Vec::new();
    let mut chosen: Vec<Option<usize>> = vec![None; desired.len()];

    for (slot, cands) in candidates.iter().enumerate() {
        if let Some(&i) = cands
            .iter()
            .find(|&&i| topo.paths[i].flags & DISPLAYCONFIG_PATH_ACTIVE != 0)
        {
            chosen[slot] = Some(i);
            used.push(topo.paths[i].sourceInfo.id);
        }
    }
    for (slot, cands) in candidates.iter().enumerate() {
        if chosen[slot].is_some() {
            continue;
        }
        match cands
            .iter()
            .find(|&&i| !used.contains(&topo.paths[i].sourceInfo.id))
        {
            Some(&i) => {
                chosen[slot] = Some(i);
                used.push(topo.paths[i].sourceInfo.id);
            }
            None => bail!(
                "no free GPU source for target {} — the adapter cannot drive {} outputs at once",
                desired[slot].target_id,
                desired.len()
            ),
        }
    }

    Ok(chosen
        .into_iter()
        .map(|i| {
            let mut path = topo.paths[i.expect("every slot was filled above")];
            path.flags = DISPLAYCONFIG_PATH_ACTIVE;
            // Opt out of virtual mode so the union stays a plain index rather
            // than the cloneGroupId/sourceModeInfoIdx bitfield pair.
            path.flags &= !DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE;
            path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            path
        })
        .collect())
}

/// Alternate between two profiles: whichever one isn't currently active wins.
pub fn switch(config: &Config, a: &str, b: &str, dry_run: bool) -> Result<(String, Outcome)> {
    for name in [a, b] {
        if !config.profiles.contains_key(name) {
            let known: Vec<&str> = config.profiles.keys().map(|s| s.as_str()).collect();
            bail!(
                "no profile named \"{name}\"; known profiles: {}",
                known.join(", ")
            );
        }
    }

    let target = match current_profile(config)?.as_deref() {
        Some(cur) if cur == a => b,
        Some(cur) if cur == b => a,
        // Neither profile matches what's on screen — some other arrangement.
        // Going to the first named profile is the useful move, and it's what
        // makes a hotkey recover from an odd state rather than refusing.
        _ => a,
    };
    let outcome = apply_profile(config, target, dry_run)?;
    Ok((target.to_string(), outcome))
}
