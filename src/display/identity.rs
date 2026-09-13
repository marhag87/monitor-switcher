//! Stable monitor identity.
//!
//! The whole design rests on finding a key for a physical output that survives
//! that output being switched off. EDID-derived ids (Monitor ID, serial number)
//! do not: once a display goes inactive Windows reports them blank, which is why
//! the GDI-based tools cannot re-enable a display they just disabled.
//!
//! `(adapter device path, target id)` does survive, because the connector's
//! hot-plug-detect line stays live on the cable regardless of whether anything
//! is being driven over it.
//!
//! Note what is deliberately *not* part of the key: the adapter `LUID`. A LUID
//! is only unique until the next reboot — Windows regenerates it on restart and
//! on any driver stop/start — so it is resolved fresh on every run and never
//! written to disk.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Gdi::DISPLAYCONFIG_PATH_ACTIVE;

use super::ccd::{DeviceNames, SourceMode, TargetName, Topology};

/// The persistent identity of a physical output. This is what goes in the
/// config file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetKey {
    /// PCI device path of the GPU, e.g. `\\?\PCI#VEN_10DE&DEV_2705#...`.
    pub adapter: String,
    /// Target id of the output on that adapter.
    pub target_id: u32,
}

/// A physical output as it exists right now.
pub struct Monitor {
    pub key: TargetKey,
    /// Live LUID for this run only — never persisted.
    pub adapter_luid: LUID,
    pub source_id: u32,
    pub active: bool,
    pub available: bool,
    pub name: TargetName,
    pub mode: Option<SourceMode>,
    pub refresh_hz: Option<f64>,
    pub gdi_name: Option<String>,
}

impl Monitor {
    /// Best human-readable label available, for matching by eye in `list`.
    pub fn label(&self) -> String {
        match (&self.name.friendly, &self.name.edid) {
            (Some(f), Some(e)) => format!("{f} [{e}]"),
            (Some(f), None) => f.clone(),
            (None, Some(e)) => e.clone(),
            (None, None) => "(unidentified)".to_string(),
        }
    }
}

/// Collapse a `QDC_ALL_PATHS` result into one entry per physical output.
///
/// That query returns roughly source x target combinations, so a single monitor
/// appears many times. Where a target has several candidate paths we keep the
/// active one, since only it carries mode information.
pub fn enumerate(topo: &Topology, names: &mut dyn DeviceNames) -> Result<Vec<Monitor>> {
    let mut out: Vec<Monitor> = Vec::new();

    for path in &topo.paths {
        let active = path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0;
        let available = path.targetInfo.targetAvailable.as_bool();

        // Targets the GPU does not report as available are phantom entries.
        if !available && !active {
            continue;
        }

        let adapter = names.adapter_path(path.targetInfo.adapterId)?;
        let key = TargetKey {
            adapter,
            target_id: path.targetInfo.id,
        };

        // Keep the active path if we already have this target from an inactive
        // one; otherwise skip the duplicate.
        if let Some(existing) = out.iter_mut().find(|m| m.key == key) {
            if active && !existing.active {
                *existing = build(path, key, topo, names)?;
            }
            continue;
        }
        out.push(build(path, key, topo, names)?);
    }

    // Active displays first, then by target id, so the listing is stable run to
    // run rather than following the API's path order.
    out.sort_by_key(|m| (!m.active, m.key.target_id));
    Ok(out)
}

fn build(
    path: &windows::Win32::Devices::Display::DISPLAYCONFIG_PATH_INFO,
    key: TargetKey,
    topo: &Topology,
    names: &mut dyn DeviceNames,
) -> Result<Monitor> {
    let active = path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0;
    let adapter_luid = path.targetInfo.adapterId;
    let source_id = path.sourceInfo.id;

    let refresh = path.targetInfo.refreshRate;
    let refresh_hz = if refresh.Denominator != 0 && refresh.Numerator != 0 {
        Some(refresh.Numerator as f64 / refresh.Denominator as f64)
    } else {
        None
    };

    Ok(Monitor {
        key,
        adapter_luid,
        source_id,
        active,
        available: path.targetInfo.targetAvailable.as_bool(),
        name: names.target_name(adapter_luid, path.targetInfo.id)?,
        mode: if active { topo.source_mode(path) } else { None },
        refresh_hz,
        gdi_name: if active {
            super::ccd::source_gdi_name(path.sourceInfo.adapterId, source_id)
        } else {
            None
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::ccd::{test_path, FixedNames};
    use std::collections::HashMap;

    const GPU: u32 = 1;

    fn names() -> FixedNames {
        FixedNames {
            adapters: HashMap::from([(GPU, "adapter-0".to_string())]),
            targets: HashMap::new(),
        }
    }

    fn named(target_id: u32, friendly: &str, edid: &str) -> ((u32, u32), TargetName) {
        (
            (GPU, target_id),
            TargetName {
                friendly: Some(friendly.to_string()),
                device_path: None,
                edid: Some(edid.to_string()),
                output_technology: 0,
            },
        )
    }

    fn topology(paths: Vec<windows::Win32::Devices::Display::DISPLAYCONFIG_PATH_INFO>) -> Topology {
        Topology {
            paths,
            modes: Vec::new(),
        }
    }

    /// `QDC_ALL_PATHS` reports roughly every source-by-target pairing, so one
    /// monitor turns up many times. Collapsing that is the whole job.
    #[test]
    fn enumerate_reports_one_entry_per_physical_output() {
        let topo = topology(vec![
            test_path(GPU, 100, 0, false, true),
            test_path(GPU, 100, 1, false, true),
            test_path(GPU, 100, 2, false, true),
            test_path(GPU, 101, 0, false, true),
            test_path(GPU, 101, 1, false, true),
        ]);
        let found = enumerate(&topo, &mut names()).unwrap();
        assert_eq!(
            found.iter().map(|m| m.key.target_id).collect::<Vec<_>>(),
            [100, 101]
        );
    }

    /// Only the active path carries mode information, so where a target has
    /// both it is the active one that must win — whichever order they arrive in.
    #[test]
    fn enumerate_keeps_the_active_path_for_a_target() {
        for paths in [
            vec![
                test_path(GPU, 100, 0, false, true),
                test_path(GPU, 100, 1, true, true),
            ],
            vec![
                test_path(GPU, 100, 1, true, true),
                test_path(GPU, 100, 0, false, true),
            ],
        ] {
            let found = enumerate(&topology(paths), &mut names()).unwrap();
            assert_eq!(found.len(), 1);
            assert!(found[0].active, "the inactive path won");
            assert_eq!(found[0].source_id, 1);
        }
    }

    /// Targets the GPU does not report as available are phantom connectors —
    /// unless they are somehow active, in which case they plainly exist.
    #[test]
    fn enumerate_drops_phantom_targets_but_keeps_active_ones() {
        let topo = topology(vec![
            test_path(GPU, 100, 0, false, true),
            test_path(GPU, 200, 1, false, false),
            test_path(GPU, 300, 2, true, false),
        ]);
        let found = enumerate(&topo, &mut names()).unwrap();
        let ids: Vec<u32> = found.iter().map(|m| m.key.target_id).collect();
        assert!(ids.contains(&100), "{ids:?}");
        assert!(
            !ids.contains(&200),
            "a phantom target was reported: {ids:?}"
        );
        assert!(ids.contains(&300), "an active target was dropped: {ids:?}");
    }

    /// Active first, then by target id — so the listing does not reshuffle
    /// itself between runs just because the API changed its path order.
    #[test]
    fn enumerate_sorts_active_first_then_by_target_id() {
        let topo = topology(vec![
            test_path(GPU, 300, 0, false, true),
            test_path(GPU, 100, 1, false, true),
            test_path(GPU, 400, 2, true, true),
            test_path(GPU, 200, 3, true, true),
        ]);
        let found = enumerate(&topo, &mut names()).unwrap();
        assert_eq!(
            found.iter().map(|m| m.key.target_id).collect::<Vec<_>>(),
            [200, 400, 100, 300]
        );
    }

    #[test]
    fn enumerate_labels_outputs_from_the_names_it_is_given() {
        let mut names = names();
        names.targets = HashMap::from([
            named(100, "MPG321UX OLED", "MSI3DD2"),
            named(101, "DELL U2415", "DELA0BC"),
        ]);
        let topo = topology(vec![
            test_path(GPU, 100, 0, false, true),
            test_path(GPU, 101, 1, false, true),
            test_path(GPU, 102, 2, false, true),
        ]);
        let found = enumerate(&topo, &mut names).unwrap();
        assert_eq!(found[0].label(), "MPG321UX OLED [MSI3DD2]");
        assert_eq!(found[1].label(), "DELL U2415 [DELA0BC]");
        // A target that reports nothing is exactly what an output that is
        // switched off looks like.
        assert_eq!(found[2].label(), "(unidentified)");
    }

    /// An adapter that cannot be resolved is fatal: without its device path
    /// there is no stable identity to key a config on.
    #[test]
    fn enumerate_fails_when_an_adapter_cannot_be_resolved() {
        let topo = topology(vec![test_path(GPU + 9, 100, 0, false, true)]);
        assert!(enumerate(&topo, &mut names()).is_err());
    }

    #[test]
    fn enumerate_of_nothing_is_empty_rather_than_an_error() {
        assert!(enumerate(&topology(Vec::new()), &mut names())
            .unwrap()
            .is_empty());
    }
}
