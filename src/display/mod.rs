//! Display topology: enumeration, identity, and applying profiles.
//!
//! This module owns the CCD API. A future DDC/CI module (monitor input
//! switching, VCP features) is a sibling, not a submodule — it talks to a
//! different Win32 surface and shares only monitor identity.

pub mod apply;
pub mod ccd;
pub mod identity;

pub use identity::{Monitor, TargetKey};

use anyhow::Result;
use windows::Win32::Devices::Display::QDC_ALL_PATHS;

/// Enumerate every physically connected output, active or not.
///
/// `QDC_ALL_PATHS` is the load-bearing detail: unlike the active-only query that
/// most tools use, it reports outputs that are currently switched off, which is
/// what makes re-enabling one possible.
pub fn enumerate() -> Result<Vec<Monitor>> {
    let topo = ccd::query(QDC_ALL_PATHS)?;
    identity::enumerate(&topo, &mut ccd::SystemNames::default())
}
