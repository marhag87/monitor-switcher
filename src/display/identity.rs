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

use super::ccd::{self, AdapterPaths, SourceMode, TargetName, Topology};

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
pub fn enumerate(topo: &Topology) -> Result<Vec<Monitor>> {
    let mut adapters = AdapterPaths::default();
    let mut out: Vec<Monitor> = Vec::new();

    for path in &topo.paths {
        let active = path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0;
        let available = path.targetInfo.targetAvailable.as_bool();

        // Targets the GPU does not report as available are phantom entries.
        if !available && !active {
            continue;
        }

        let adapter = adapters.get(path.targetInfo.adapterId)?;
        let key = TargetKey {
            adapter,
            target_id: path.targetInfo.id,
        };

        // Keep the active path if we already have this target from an inactive
        // one; otherwise skip the duplicate.
        if let Some(existing) = out.iter_mut().find(|m| m.key == key) {
            if active && !existing.active {
                *existing = build(path, key, topo)?;
            }
            continue;
        }
        out.push(build(path, key, topo)?);
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
        name: ccd::target_name(adapter_luid, path.targetInfo.id)?,
        mode: if active { topo.source_mode(path) } else { None },
        refresh_hz,
        gdi_name: if active {
            ccd::source_gdi_name(path.sourceInfo.adapterId, source_id)
        } else {
            None
        },
    })
}
