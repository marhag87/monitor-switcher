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
};

use super::ccd::{self, DeviceNames};
use super::identity::{self, TargetKey};
#[cfg(feature = "cec")]
use crate::cec::Cec;
use crate::config::{CecConfig, Config};
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

fn active_keys() -> Result<Vec<TargetKey>> {
    Ok(super::enumerate()?
        .into_iter()
        .filter(|m| m.active)
        .map(|m| m.key)
        .collect())
}

/// Set equality: order-insensitive, and unbothered by repeats on either side.
///
/// Not a length check plus one-way containment — that calls `[x, x]` and
/// `[x, y]` equal, since the lengths match and every element of the first is
/// present in the second. These lists are at most a handful of displays long,
/// so the quadratic comparison costs nothing worth avoiding.
fn same_set(a: &[TargetKey], b: &[TargetKey]) -> bool {
    a.iter().all(|k| b.contains(k)) && b.iter().all(|k| a.contains(k))
}

/// Resolve profile member names to the outputs they refer to, in first-mention
/// order and without repeats.
///
/// Collapsing repeats is what makes a profile a *set* of outputs, which is all
/// the README ever promises one to be. A target can be named twice directly, or
/// reached through two config names that point at the same output; either way
/// it means the same thing as naming it once.
///
/// Passing repeats through would reach `build_paths`, which hands each wanted
/// output its own GPU source and would find nothing to give the second mention.
/// That failed as "target N is not connected" — directly above a line listing
/// target N as connected.
fn resolve(config: &Config, members: &[String]) -> Result<Vec<TargetKey>> {
    let mut keys: Vec<TargetKey> = Vec::with_capacity(members.len());
    for name in members {
        let key = config
            .targets
            .get(name)
            .map(|e| e.key.clone())
            .ok_or_else(|| {
                let known: Vec<&str> = config.targets.keys().map(|s| s.as_str()).collect();
                anyhow::anyhow!(
                    "profile refers to unknown target \"{name}\"; known targets: {}",
                    known.join(", ")
                )
            })?;
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    Ok(keys)
}

/// The result of applying a profile: what happened to the display topology,
/// plus whatever CEC power control had to say about it.
pub struct Report {
    pub outcome: Outcome,
    /// Human-readable notes from CEC actions, including failures. These never
    /// abort an apply — the topology change is the part that must work.
    pub cec: Vec<String>,
}

pub fn apply_profile(config: &Config, profile: &str, dry_run: bool) -> Result<Report> {
    let desired = resolve(config, members_of(config, profile)?)?;

    // `apply_targets` has to read the topology anyway, and hands back the set
    // it found active. Asking separately would be a second enumeration of the
    // same thing, and a second chance for the two answers to disagree.
    let (outcome, was_active) = apply_targets(&desired, dry_run)?;

    let cec = if dry_run || matches!(outcome, Outcome::AlreadyActive) {
        Vec::new()
    } else {
        run_cec(config, &desired, &was_active)
    };

    Ok(Report { outcome, cec })
}

/// The CEC-capable displays this change should wake, and those it should put to
/// sleep — only ones whose active state actually changes, and only where the
/// config opts into that direction.
///
/// Shared by both builds so that the feature-off warning names exactly the
/// displays a feature-on build would have acted on, rather than every display
/// that merely has a `cec` block.
fn cec_transitions<'a>(
    config: &'a Config,
    desired: &[TargetKey],
    was_active: &[TargetKey],
) -> CecPlan<'a> {
    let mut plan = CecPlan::default();
    for (name, entry) in &config.targets {
        let Some(conf) = entry.cec.as_ref() else {
            continue;
        };
        let wanted = desired.contains(&entry.key);
        let had = was_active.contains(&entry.key);
        if wanted && !had && conf.power_on {
            plan.waking.push((name.as_str(), conf));
        } else if had && !wanted && conf.standby {
            plan.sleeping.push((name.as_str(), conf));
        }
    }
    plan
}

/// A configured display CEC should act on, by name.
type CecTarget<'a> = (&'a str, &'a CecConfig);

/// Named rather than a pair of bare `Vec`s, whose identical types make them easy
/// to hand over in the wrong order.
#[derive(Default)]
struct CecPlan<'a> {
    waking: Vec<CecTarget<'a>>,
    sleeping: Vec<CecTarget<'a>>,
}

/// Power displays on or off to match the topology we just applied.
///
/// Ordering is deliberate: this runs *after* the topology change, never before.
/// If an apply fails we must not have already switched someone's TV off for a
/// change that never happened.
#[cfg(feature = "cec")]
fn run_cec(config: &Config, desired: &[TargetKey], was_active: &[TargetKey]) -> Vec<String> {
    let mut notes = Vec::new();
    let plan = cec_transitions(config, desired, was_active);

    for (name, conf) in plan.sleeping {
        notes.push(match Cec::open(conf.hdmi_port).and_then(|c| c.sleep()) {
            Ok(msg) => format!("{name}: {msg}"),
            Err(e) => format!("{name}: CEC standby failed: {e:#}"),
        });
    }
    for (name, conf) in plan.waking {
        notes.push(
            match Cec::open(conf.hdmi_port).and_then(|c| c.wake(conf.activate_source)) {
                Ok(msg) => format!("{name}: {msg}"),
                Err(e) => format!("{name}: CEC power-on failed: {e:#}"),
            },
        );
    }
    notes
}

/// Without the `cec` feature there is no power control — but a config written
/// for a build that had it still describes some, and silently ignoring that
/// would be the kind of quiet no-op this tool exists to avoid.
///
/// Only displays this change would actually have powered are worth mentioning.
/// Naming every display that merely has a `cec` block would print the same
/// warning on switches that were never going to touch the TV.
#[cfg(not(feature = "cec"))]
fn run_cec(config: &Config, desired: &[TargetKey], was_active: &[TargetKey]) -> Vec<String> {
    let plan = cec_transitions(config, desired, was_active);
    let affected: Vec<&str> = plan
        .waking
        .iter()
        .chain(&plan.sleeping)
        .map(|(name, _)| *name)
        .collect();
    if affected.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "{} would have been powered over CEC, but this build has the \"cec\" feature off",
        affected.join(", ")
    )]
}

/// Make exactly `desired` the active outputs.
///
/// Returns the set that was active beforehand alongside the outcome: this
/// function has to read the topology regardless, so the caller need not read it
/// a second time to find out what changed.
fn apply_targets(desired: &[TargetKey], dry_run: bool) -> Result<(Outcome, Vec<TargetKey>)> {
    let topo = ccd::query(QDC_ALL_PATHS)?;
    // One resolver for the whole call, so the adapter paths it memoises are
    // shared between enumerating and building the new path array.
    let mut names = ccd::SystemNames::default();
    let monitors = identity::enumerate(&topo, &mut names)?;

    let active: Vec<TargetKey> = monitors
        .iter()
        .filter(|m| m.active)
        .map(|m| m.key.clone())
        .collect();
    if !dry_run && same_set(desired, &active) {
        return Ok((Outcome::AlreadyActive, active));
    }

    let paths = build_paths(&topo, desired, &mut names)?;

    // Validate first either way — a dry run stops here, and a real apply gets a
    // clearer error from the validate call than from a half-applied change.
    let validate_flags = SDC_VALIDATE | SDC_TOPOLOGY_SUPPLIED | SDC_ALLOW_PATH_ORDER_CHANGES;
    let rc = ccd::set(&paths, None, validate_flags);
    if dry_run {
        return if rc == ERROR_SUCCESS {
            Ok((Outcome::Validated, active))
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
        return Ok((Outcome::Applied(Tier::Database), active));
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
        return Ok((Outcome::Applied(Tier::BestMode), active));
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
    names: &mut dyn DeviceNames,
) -> Result<Vec<DISPLAYCONFIG_PATH_INFO>> {
    // Candidate paths per desired target. QDC_ALL_PATHS lists roughly every
    // source x target pairing, so each target has several, differing in which
    // GPU source feeds it.
    let mut candidates: Vec<Vec<usize>> = vec![Vec::new(); desired.len()];
    for (i, path) in topo.paths.iter().enumerate() {
        if !path.targetInfo.targetAvailable.as_bool() {
            continue;
        }
        let key = TargetKey {
            adapter: names.adapter_path(path.targetInfo.adapterId)?,
            target_id: path.targetInfo.id,
        };
        if let Some(slot) = desired.iter().position(|d| *d == key) {
            candidates[slot].push(i);
        }
    }

    for (slot, cands) in candidates.iter().enumerate() {
        if cands.is_empty() {
            let key = &desired[slot];
            let present: Vec<String> = identity::enumerate(topo, names)?
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
            // Exactly ACTIVE, and in particular not
            // DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE: leaving that one out is
            // what keeps the mode-index union a plain index rather than the
            // cloneGroupId/sourceModeInfoIdx bitfield pair the reader below
            // would then have to decode.
            path.flags = DISPLAYCONFIG_PATH_ACTIVE;
            path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            path
        })
        .collect())
}

/// Alternate between two profiles: whichever one isn't currently active wins.
pub fn switch(config: &Config, a: &str, b: &str, dry_run: bool) -> Result<(String, Report)> {
    // Both names are checked before anything is read or applied, so a typo in
    // either one fails without touching the displays.
    let a_members = members_of(config, a)?;
    members_of(config, b)?;

    // Ask whether `a` is what's on screen, rather than searching the config for
    // whichever profile name matches the screen. That search returns the first
    // match in name order, so a third profile describing the same outputs as
    // `a` or `b` would win it — leaving neither `a` nor `b` matched, and the
    // fallback re-applying the profile that was already active. A hotkey bound
    // to `switch` would then do nothing at all.
    let on_a = same_set(&resolve(config, a_members)?, &active_keys()?);

    // Anything that isn't `a` — `b`, or some third arrangement entirely — sends
    // us to `a`, so a hotkey recovers from an odd state rather than refusing.
    let target = if on_a { b } else { a };

    let outcome = apply_profile(config, target, dry_run)?;
    Ok((target.to_string(), outcome))
}

/// The target names a profile lists, or an error naming the profiles that exist.
fn members_of<'a>(config: &'a Config, profile: &str) -> Result<&'a [String]> {
    config
        .profiles
        .get(profile)
        .map(Vec::as_slice)
        .ok_or_else(|| {
            let known: Vec<&str> = config.profiles.keys().map(|s| s.as_str()).collect();
            anyhow::anyhow!(
                "no profile named \"{profile}\"; known profiles: {}",
                known.join(", ")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CecConfig, TargetEntry};

    fn key(target_id: u32) -> TargetKey {
        TargetKey {
            adapter: "adapter-0".into(),
            target_id,
        }
    }

    fn config(targets: &[(&str, u32)]) -> Config {
        Config {
            targets: targets
                .iter()
                .map(|(name, id)| {
                    let entry = TargetEntry {
                        key: key(*id),
                        edid: None,
                        friendly: None,
                        cec: None,
                    };
                    (name.to_string(), entry)
                })
                .collect(),
            ..Config::default()
        }
    }

    fn members(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolve_collapses_a_repeated_member() {
        let cfg = config(&[("main", 1), ("sidemon", 2)]);
        let got = resolve(&cfg, &members(&["main", "sidemon", "main"])).unwrap();
        assert_eq!(got, vec![key(1), key(2)]);
    }

    /// Two config names may point at one output; that is still one output.
    #[test]
    fn resolve_collapses_two_names_for_the_same_output() {
        let cfg = config(&[("television", 1), ("tv", 1)]);
        let got = resolve(&cfg, &members(&["tv", "television"])).unwrap();
        assert_eq!(got, vec![key(1)]);
    }

    #[test]
    fn resolve_keeps_first_mention_order() {
        let cfg = config(&[("a", 1), ("b", 2), ("c", 3)]);
        let got = resolve(&cfg, &members(&["c", "a", "c", "b"])).unwrap();
        assert_eq!(got, vec![key(3), key(1), key(2)]);
    }

    #[test]
    fn resolve_names_both_the_unknown_target_and_the_known_ones() {
        let cfg = config(&[("main", 1)]);
        let err = resolve(&cfg, &members(&["main", "nope"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope"), "{err}");
        assert!(err.contains("main"), "{err}");
    }

    #[test]
    fn same_set_ignores_order() {
        assert!(same_set(&[key(1), key(2)], &[key(2), key(1)]));
    }

    /// The regression guard: a length check plus one-way containment called
    /// these two equal.
    #[test]
    fn same_set_rejects_a_different_set_of_equal_length() {
        assert!(!same_set(&[key(1), key(1)], &[key(1), key(2)]));
        assert!(!same_set(&[key(1), key(2)], &[key(1), key(1)]));
    }

    #[test]
    fn same_set_tolerates_repeats_on_either_side() {
        assert!(same_set(&[key(1), key(1), key(2)], &[key(2), key(1)]));
        assert!(same_set(&[key(2), key(1)], &[key(1), key(1), key(2)]));
    }

    #[test]
    fn same_set_rejects_a_subset_in_either_direction() {
        assert!(!same_set(&[key(1)], &[key(1), key(2)]));
        assert!(!same_set(&[key(1), key(2)], &[key(1)]));
    }

    // --- build_paths, driven by a topology assembled here ---

    const GPU: u32 = 1;

    fn resolver() -> ccd::FixedNames {
        ccd::FixedNames {
            adapters: std::collections::HashMap::from([(GPU, "adapter-0".to_string())]),
            targets: std::collections::HashMap::new(),
        }
    }

    fn topology(paths: Vec<DISPLAYCONFIG_PATH_INFO>) -> ccd::Topology {
        ccd::Topology {
            paths,
            modes: Vec::new(),
        }
    }

    fn mode_indices(path: &DISPLAYCONFIG_PATH_INFO) -> (u32, u32) {
        // SAFETY: the same union build_paths just wrote through; no
        // virtual-mode flag is set, so both members are plain indices.
        unsafe {
            (
                path.sourceInfo.Anonymous.modeInfoIdx,
                path.targetInfo.Anonymous.modeInfoIdx,
            )
        }
    }

    fn sources_for(paths: &[DISPLAYCONFIG_PATH_INFO]) -> Vec<u32> {
        paths.iter().map(|p| p.sourceInfo.id).collect()
    }

    #[test]
    fn build_paths_returns_one_path_per_wanted_output_in_order() {
        let topo = topology(vec![
            ccd::test_path(GPU, 100, 0, false, true),
            ccd::test_path(GPU, 101, 1, false, true),
        ]);
        let built = build_paths(&topo, &[key(101), key(100)], &mut resolver()).unwrap();
        assert_eq!(
            built.iter().map(|p| p.targetInfo.id).collect::<Vec<_>>(),
            [101, 100]
        );
    }

    /// Every path handed to `SetDisplayConfig` must say ACTIVE and nothing else
    /// — in particular not SUPPORT_VIRTUAL_MODE, which would turn the mode-index
    /// union into a bitfield pair — and must carry no stale mode indices.
    #[test]
    fn build_paths_marks_paths_active_and_clears_their_modes() {
        let mut noisy = ccd::test_path(GPU, 100, 0, false, true);
        noisy.flags = u32::MAX;
        noisy.sourceInfo.Anonymous.modeInfoIdx = 7;
        noisy.targetInfo.Anonymous.modeInfoIdx = 9;

        let built = build_paths(&topology(vec![noisy]), &[key(100)], &mut resolver()).unwrap();

        assert_eq!(built[0].flags, DISPLAYCONFIG_PATH_ACTIVE);
        assert_eq!(
            mode_indices(&built[0]),
            (
                DISPLAYCONFIG_PATH_MODE_IDX_INVALID,
                DISPLAYCONFIG_PATH_MODE_IDX_INVALID
            )
        );
    }

    /// A display that is already on keeps the source it is already using, so a
    /// swap disturbs the displays that are not changing as little as possible.
    #[test]
    fn build_paths_leaves_an_active_output_on_its_current_source() {
        let topo = topology(vec![
            ccd::test_path(GPU, 100, 0, false, true),
            ccd::test_path(GPU, 100, 1, false, true),
            ccd::test_path(GPU, 100, 2, true, true),
            ccd::test_path(GPU, 101, 0, false, true),
            ccd::test_path(GPU, 101, 1, false, true),
        ]);
        let built = build_paths(&topo, &[key(100), key(101)], &mut resolver()).unwrap();
        assert_eq!(sources_for(&built), [2, 0]);
    }

    #[test]
    fn build_paths_gives_each_output_a_source_of_its_own() {
        let topo = topology(vec![
            ccd::test_path(GPU, 100, 0, false, true),
            ccd::test_path(GPU, 100, 1, false, true),
            ccd::test_path(GPU, 101, 0, false, true),
            ccd::test_path(GPU, 101, 1, false, true),
        ]);
        let built = build_paths(&topo, &[key(100), key(101)], &mut resolver()).unwrap();
        let mut sources = sources_for(&built);
        sources.sort_unstable();
        assert_eq!(sources, [0, 1], "two outputs shared one source");
    }

    #[test]
    fn build_paths_reports_an_output_that_is_not_connected() {
        let topo = topology(vec![ccd::test_path(GPU, 100, 0, false, true)]);
        let err = build_paths(&topo, &[key(100), key(999)], &mut resolver())
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(err.contains("999"), "{err}");
        assert!(err.contains("not connected"), "{err}");
    }

    /// A path the GPU marks unavailable is not a route to that output.
    #[test]
    fn build_paths_will_not_route_through_an_unavailable_path() {
        let topo = topology(vec![
            ccd::test_path(GPU, 100, 0, false, true),
            ccd::test_path(GPU, 101, 1, false, false),
        ]);
        let err = build_paths(&topo, &[key(100), key(101)], &mut resolver())
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(err.contains("101"), "{err}");
    }

    /// The output ceiling: more displays wanted than the adapter has sources to
    /// drive them with.
    #[test]
    fn build_paths_reports_when_the_sources_run_out() {
        let topo = topology(vec![
            ccd::test_path(GPU, 100, 0, false, true),
            ccd::test_path(GPU, 101, 0, false, true),
        ]);
        let err = build_paths(&topo, &[key(100), key(101)], &mut resolver())
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no free GPU source"), "{err}");
        assert!(err.contains("2 outputs at once"), "{err}");
    }

    #[test]
    fn build_paths_of_nothing_asks_for_nothing() {
        let topo = topology(vec![ccd::test_path(GPU, 100, 0, true, true)]);
        assert!(build_paths(&topo, &[], &mut resolver()).unwrap().is_empty());
    }

    fn cec_config(power_on: bool, standby: bool) -> CecConfig {
        CecConfig {
            hdmi_port: 1,
            power_on,
            standby,
            activate_source: true,
        }
    }

    /// A config with one CEC-capable display (`tv`, target 1) and one plain one
    /// (`main`, target 2).
    fn cec_config_with(cec: Option<CecConfig>) -> Config {
        let entry = |target_id, cec| TargetEntry {
            key: key(target_id),
            edid: None,
            friendly: None,
            cec,
        };
        let mut config = Config::default();
        config.targets.insert("tv".to_string(), entry(1, cec));
        config.targets.insert("main".to_string(), entry(2, None));
        config
    }

    fn names<'a>(targets: &[CecTarget<'a>]) -> Vec<&'a str> {
        targets.iter().map(|(name, _)| *name).collect()
    }

    #[test]
    fn a_display_being_switched_on_is_woken() {
        let config = cec_config_with(Some(cec_config(true, true)));
        let plan = cec_transitions(&config, &[key(1), key(2)], &[key(2)]);
        assert_eq!(names(&plan.waking), ["tv"]);
        assert!(names(&plan.sleeping).is_empty());
    }

    #[test]
    fn a_display_being_switched_off_is_put_to_sleep() {
        let config = cec_config_with(Some(cec_config(true, true)));
        let plan = cec_transitions(&config, &[key(2)], &[key(1), key(2)]);
        assert_eq!(names(&plan.sleeping), ["tv"]);
        assert!(names(&plan.waking).is_empty());
    }

    /// The cleanup this guards: a change that leaves the TV where it was must
    /// produce no CEC work, and so no "feature is off" warning either.
    #[test]
    fn a_display_that_does_not_change_state_is_left_alone() {
        let config = cec_config_with(Some(cec_config(true, true)));

        let still_on = cec_transitions(&config, &[key(1), key(2)], &[key(1), key(2)]);
        assert!(names(&still_on.waking).is_empty() && names(&still_on.sleeping).is_empty());

        let still_off = cec_transitions(&config, &[key(2)], &[key(2)]);
        assert!(names(&still_off.waking).is_empty() && names(&still_off.sleeping).is_empty());
    }

    #[test]
    fn each_direction_can_be_opted_out_of_separately() {
        let no_wake = cec_config_with(Some(cec_config(false, true)));
        assert!(names(&cec_transitions(&no_wake, &[key(1)], &[]).waking).is_empty());
        assert_eq!(
            names(&cec_transitions(&no_wake, &[], &[key(1)]).sleeping),
            ["tv"]
        );

        let no_sleep = cec_config_with(Some(cec_config(true, false)));
        assert_eq!(
            names(&cec_transitions(&no_sleep, &[key(1)], &[]).waking),
            ["tv"]
        );
        assert!(names(&cec_transitions(&no_sleep, &[], &[key(1)]).sleeping).is_empty());
    }

    #[test]
    fn a_display_without_a_cec_block_is_never_acted_on() {
        let config = cec_config_with(None);
        let plan = cec_transitions(&config, &[key(1)], &[key(2)]);
        assert!(names(&plan.waking).is_empty() && names(&plan.sleeping).is_empty());
    }

    #[test]
    fn same_set_matches_two_empty_sets() {
        assert!(same_set(&[], &[]));
        assert!(!same_set(&[], &[key(1)]));
    }
}
