//! Safe wrappers over the CCD (Connecting and Configuring Displays) API.
//!
//! All `unsafe` for display topology lives here. Callers above this module deal
//! in plain Rust values.

use std::collections::HashMap;
use std::mem::size_of;

use anyhow::{bail, Result};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, SetDisplayConfig,
    DISPLAYCONFIG_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY, QUERY_DISPLAY_CONFIG_FLAGS, SET_DISPLAY_CONFIG_FLAGS,
};
use windows::Win32::Foundation::LUID;

use crate::winerr;

const ERROR_SUCCESS: u32 = 0;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

/// A snapshot of the display configuration as returned by `QueryDisplayConfig`.
pub struct Topology {
    pub paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    pub modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

impl Topology {
    /// The source mode (resolution + desktop position) a path points at, if it
    /// has one. Inactive paths generally do not.
    pub fn source_mode(&self, path: &DISPLAYCONFIG_PATH_INFO) -> Option<SourceMode> {
        // SAFETY: reading the union's plain index. We never set
        // DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE, and we do not pass
        // QDC_VIRTUAL_MODE_AWARE when querying, so the union is a plain u32.
        let idx = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
        let info = self.modes.get(idx)?;
        if info.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
            return None;
        }
        // SAFETY: infoType says this union member is the active one.
        let m = unsafe { info.Anonymous.sourceMode };
        Some(SourceMode {
            width: m.width,
            height: m.height,
            x: m.position.x,
            y: m.position.y,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceMode {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
}

/// Query the display configuration.
///
/// `GetDisplayConfigBufferSizes` and `QueryDisplayConfig` are not atomic with
/// respect to each other: a hotplug between the two invalidates the sizes and
/// the second call answers `ERROR_INSUFFICIENT_BUFFER`. Retry a few times.
pub fn query(flags: QUERY_DISPLAY_CONFIG_FLAGS) -> Result<Topology> {
    for _ in 0..5 {
        let (mut n_paths, mut n_modes) = (0u32, 0u32);
        // SAFETY: both out-params are valid pointers to initialised locals.
        let rc = unsafe { GetDisplayConfigBufferSizes(flags, &mut n_paths, &mut n_modes) };
        if rc.0 != ERROR_SUCCESS {
            bail!("GetDisplayConfigBufferSizes failed: {}", winerr::describe(rc.0));
        }

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        // SAFETY: the buffers are sized by the counts we just received and are
        // passed with those same counts, which QueryDisplayConfig updates to the
        // number actually written.
        let rc = unsafe {
            QueryDisplayConfig(
                flags,
                &mut n_paths,
                paths.as_mut_ptr(),
                &mut n_modes,
                modes.as_mut_ptr(),
                None,
            )
        };
        match rc.0 {
            ERROR_SUCCESS => {
                paths.truncate(n_paths as usize);
                modes.truncate(n_modes as usize);
                return Ok(Topology { paths, modes });
            }
            ERROR_INSUFFICIENT_BUFFER => continue,
            code => bail!("QueryDisplayConfig failed: {}", winerr::describe(code)),
        }
    }
    bail!("display configuration kept changing while being read (5 attempts)")
}

/// Apply (or validate) a display configuration.
///
/// Returns the raw `WIN32_ERROR` code so callers can distinguish "this tier of
/// the apply ladder didn't work" from a hard failure.
pub fn set(
    paths: &[DISPLAYCONFIG_PATH_INFO],
    modes: Option<&[DISPLAYCONFIG_MODE_INFO]>,
    flags: SET_DISPLAY_CONFIG_FLAGS,
) -> u32 {
    // SAFETY: slices are passed with their own lengths; the API only reads them.
    unsafe { SetDisplayConfig(Some(paths), modes, flags) as u32 }
}

/// Resolve an adapter LUID to its PCI device path.
///
/// This is the persistent half of a monitor's identity. The LUID itself is
/// regenerated on every boot and on driver restart, so it must never be stored;
/// this path is stable and is what we key configuration on.
pub fn adapter_device_path(adapter: LUID) -> Result<String> {
    let mut req = DISPLAYCONFIG_ADAPTER_NAME::default();
    req.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME;
    req.header.size = size_of::<DISPLAYCONFIG_ADAPTER_NAME>() as u32;
    req.header.adapterId = adapter;

    // SAFETY: req is a correctly sized, correctly typed request packet.
    let rc = unsafe { DisplayConfigGetDeviceInfo(&mut req.header) };
    if rc as u32 != ERROR_SUCCESS {
        bail!("GET_ADAPTER_NAME failed: {}", winerr::describe(rc as u32));
    }
    Ok(wide_to_string(&req.adapterDevicePath))
}

/// Memoising adapter-path resolver — one call per distinct LUID instead of one
/// per path, since `QDC_ALL_PATHS` returns many paths per adapter.
#[derive(Default)]
pub struct AdapterPaths {
    cache: HashMap<(u32, i32), String>,
}

impl AdapterPaths {
    pub fn get(&mut self, adapter: LUID) -> Result<String> {
        let key = (adapter.LowPart, adapter.HighPart);
        if let Some(p) = self.cache.get(&key) {
            return Ok(p.clone());
        }
        let path = adapter_device_path(adapter)?;
        self.cache.insert(key, path.clone());
        Ok(path)
    }
}

/// What `GET_TARGET_NAME` knows about a physical output.
///
/// Every field is best-effort: an inactive target may report nothing but its
/// output technology. Nothing here is ever used as a key — that is
/// `(adapter device path, target id)`'s job — but the EDID fields are retained
/// because DDC/CI addresses monitors by EDID id.
#[derive(Debug, Clone, Default)]
pub struct TargetName {
    pub friendly: Option<String>,
    pub device_path: Option<String>,
    pub edid: Option<String>,
    pub output_technology: i32,
}

pub fn target_name(adapter: LUID, target_id: u32) -> Result<TargetName> {
    let mut req = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
    req.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
    req.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
    req.header.adapterId = adapter;
    req.header.id = target_id;

    // SAFETY: req is a correctly sized, correctly typed request packet.
    let rc = unsafe { DisplayConfigGetDeviceInfo(&mut req.header) };
    if rc as u32 != ERROR_SUCCESS {
        // Inactive targets routinely refuse this. Not an error worth failing on
        // — it only costs us a human-readable label.
        return Ok(TargetName::default());
    }

    // SAFETY: the flags union's `value` member aliases the whole bitfield.
    let edid_ids_valid = unsafe { req.flags.Anonymous.value } & 0x2 != 0;

    Ok(TargetName {
        friendly: non_empty(wide_to_string(&req.monitorFriendlyDeviceName)),
        device_path: non_empty(wide_to_string(&req.monitorDevicePath)),
        edid: if edid_ids_valid || req.edidManufactureId != 0 {
            Some(format_edid_id(req.edidManufactureId, req.edidProductCodeId))
        } else {
            None
        },
        output_technology: req.outputTechnology.0,
    })
}

/// The GDI device name (`\\.\DISPLAY2`) for a source. Reference only: this
/// numbering is reassigned as displays come and go, which is precisely why the
/// GDI-based tools cannot address a disabled display reliably.
pub fn source_gdi_name(adapter: LUID, source_id: u32) -> Option<String> {
    let mut req = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
    req.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
    req.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
    req.header.adapterId = adapter;
    req.header.id = source_id;

    // SAFETY: req is a correctly sized, correctly typed request packet.
    let rc = unsafe { DisplayConfigGetDeviceInfo(&mut req.header) };
    if rc as u32 != ERROR_SUCCESS {
        return None;
    }
    non_empty(wide_to_string(&req.viewGdiDeviceName))
}

/// Decode the EDID manufacturer/product pair into the form every monitor tool
/// shows it in, e.g. `MSI3DD2` — three packed 5-bit letters plus a hex product
/// code. This is how ControlMyMonitor addresses a panel.
fn format_edid_id(manufacture_id: u16, product_code: u16) -> String {
    // EDID stores the manufacturer id big-endian; Windows hands it back as a
    // little-endian u16 read of those bytes.
    let packed = manufacture_id.swap_bytes();
    let letter = |shift: u16| -> char {
        let v = ((packed >> shift) & 0x1f) as u8;
        if (1..=26).contains(&v) {
            (b'A' + v - 1) as char
        } else {
            '?'
        }
    };
    format!(
        "{}{}{}{:04X}",
        letter(10),
        letter(5),
        letter(0),
        product_code
    )
}

/// Human label for a connector type.
pub fn output_technology_name(tech: i32) -> &'static str {
    use windows::Win32::Devices::Display as d;
    match DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY(tech) {
        d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HD15 => "VGA",
        d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DVI => "DVI",
        d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI => "HDMI",
        d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL => "DisplayPort",
        d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED => "eDP",
        d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_USB_TUNNEL => "DP/USB-C",
        d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL => "Internal",
        d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_MIRACAST => "Miracast",
        d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED => "Indirect",
        d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL => "Virtual",
        _ => "other",
    }
}

fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len]).trim().to_string()
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
