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
            bail!(
                "GetDisplayConfigBufferSizes failed: {}",
                winerr::describe(rc.0)
            );
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

/// Turning the opaque handles in a topology into names.
///
/// A topology is just numbers — LUIDs and target ids — and every question about
/// what they refer to is another trip into Win32. Routing those through a trait
/// keeps the code that interprets a topology separate from the code that asks
/// Windows about one, so the interpretation can be exercised against a topology
/// assembled by hand.
pub trait DeviceNames {
    fn adapter_path(&mut self, adapter: LUID) -> Result<String>;
    fn target_name(&mut self, adapter: LUID, target_id: u32) -> Result<TargetName>;
}

/// The real thing: asks Windows, and remembers adapter paths.
///
/// The memoisation is not an optimisation for its own sake — `QDC_ALL_PATHS`
/// returns many paths per adapter, so without it the same LUID is resolved
/// dozens of times per run.
#[derive(Default)]
pub struct SystemNames {
    adapters: HashMap<(u32, i32), String>,
}

impl DeviceNames for SystemNames {
    fn adapter_path(&mut self, adapter: LUID) -> Result<String> {
        let key = (adapter.LowPart, adapter.HighPart);
        if let Some(p) = self.adapters.get(&key) {
            return Ok(p.clone());
        }
        let path = adapter_device_path(adapter)?;
        self.adapters.insert(key, path.clone());
        Ok(path)
    }

    fn target_name(&mut self, adapter: LUID, target_id: u32) -> Result<TargetName> {
        target_name(adapter, target_id)
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

/// Assemble a path the way `QueryDisplayConfig` would hand one back, so a
/// topology can be built by hand. Writing a union field is safe; only reading
/// one is not, so this needs no `unsafe` of its own.
#[cfg(test)]
pub fn test_path(
    adapter: u32,
    target_id: u32,
    source_id: u32,
    active: bool,
    available: bool,
) -> DISPLAYCONFIG_PATH_INFO {
    use windows::Win32::Graphics::Gdi::{
        DISPLAYCONFIG_PATH_ACTIVE, DISPLAYCONFIG_PATH_MODE_IDX_INVALID,
    };

    let luid = LUID {
        LowPart: adapter,
        HighPart: 0,
    };
    let mut path = DISPLAYCONFIG_PATH_INFO::default();
    path.sourceInfo.adapterId = luid;
    path.sourceInfo.id = source_id;
    path.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
    path.targetInfo.adapterId = luid;
    path.targetInfo.id = target_id;
    path.targetInfo.targetAvailable = available.into();
    path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
    path.flags = if active { DISPLAYCONFIG_PATH_ACTIVE } else { 0 };
    path
}

/// Canned answers, so the topology-interpreting code can be tested without a
/// GPU. Mirrors the real thing's semantics in the one way that matters: an
/// unknown adapter is an error, while an unknown target merely has no name —
/// which is what Windows reports for an output that is switched off.
#[cfg(test)]
#[derive(Default)]
pub struct FixedNames {
    /// `LUID.LowPart` -> adapter device path.
    pub adapters: HashMap<u32, String>,
    /// `(LUID.LowPart, target id)` -> what `GET_TARGET_NAME` would answer.
    pub targets: HashMap<(u32, u32), TargetName>,
}

#[cfg(test)]
impl DeviceNames for FixedNames {
    fn adapter_path(&mut self, adapter: LUID) -> Result<String> {
        self.adapters
            .get(&adapter.LowPart)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no adapter for LUID {}", adapter.LowPart))
    }

    fn target_name(&mut self, adapter: LUID, target_id: u32) -> Result<TargetName> {
        Ok(self
            .targets
            .get(&(adapter.LowPart, target_id))
            .cloned()
            .unwrap_or_default())
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_MODE_INFO_TYPE_TARGET, DISPLAYCONFIG_SOURCE_MODE,
    };
    use windows::Win32::Foundation::POINTL;
    use windows::Win32::Graphics::Gdi::DISPLAYCONFIG_PATH_MODE_IDX_INVALID;

    /// A path whose source half points at `mode_idx`.
    ///
    /// Writing a union field is safe — only reading one is not — so a topology
    /// can be assembled here without any `unsafe` of its own, and handed to
    /// `source_mode` to exercise the reads that do need it.
    fn path_at(mode_idx: u32) -> DISPLAYCONFIG_PATH_INFO {
        let mut path = DISPLAYCONFIG_PATH_INFO::default();
        path.sourceInfo.Anonymous.modeInfoIdx = mode_idx;
        path
    }

    fn source_mode_info(width: u32, height: u32, x: i32, y: i32) -> DISPLAYCONFIG_MODE_INFO {
        let mut info = DISPLAYCONFIG_MODE_INFO {
            infoType: DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
            ..Default::default()
        };
        info.Anonymous.sourceMode = DISPLAYCONFIG_SOURCE_MODE {
            width,
            height,
            position: POINTL { x, y },
            ..Default::default()
        };
        info
    }

    #[test]
    fn source_mode_reads_the_mode_a_path_points_at() {
        let topo = Topology {
            paths: Vec::new(),
            modes: vec![
                source_mode_info(1920, 1080, 0, 0),
                source_mode_info(3840, 2160, -2560, 167),
            ],
        };

        assert_eq!(
            topo.source_mode(&path_at(0)),
            Some(SourceMode {
                width: 1920,
                height: 1080,
                x: 0,
                y: 0
            })
        );
        // Displays left of or above the primary sit at negative coordinates;
        // the position fields are signed and must stay that way.
        assert_eq!(
            topo.source_mode(&path_at(1)),
            Some(SourceMode {
                width: 3840,
                height: 2160,
                x: -2560,
                y: 167
            })
        );
    }

    #[test]
    fn source_mode_declines_an_index_past_the_end() {
        let topo = Topology {
            paths: Vec::new(),
            modes: vec![source_mode_info(1920, 1080, 0, 0)],
        };
        assert_eq!(topo.source_mode(&path_at(1)), None);
        assert_eq!(topo.source_mode(&path_at(9999)), None);
    }

    /// The value `build_paths` writes into every path it sends to Windows. It
    /// must read back as "no mode", not as an enormous index.
    #[test]
    fn source_mode_declines_the_invalid_index_sentinel() {
        let topo = Topology {
            paths: Vec::new(),
            modes: vec![source_mode_info(1920, 1080, 0, 0)],
        };
        assert_eq!(
            topo.source_mode(&path_at(DISPLAYCONFIG_PATH_MODE_IDX_INVALID)),
            None
        );
    }

    /// The union is only a source mode when `infoType` says so. Reading it as
    /// one regardless would hand back a target mode's bytes reinterpreted as a
    /// resolution — the exact mistake the discriminant check prevents.
    #[test]
    fn source_mode_declines_an_entry_that_is_not_a_source_mode() {
        let target = DISPLAYCONFIG_MODE_INFO {
            infoType: DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
            ..Default::default()
        };
        let topo = Topology {
            paths: Vec::new(),
            modes: vec![target],
        };
        assert_eq!(topo.source_mode(&path_at(0)), None);
    }

    #[test]
    fn source_mode_declines_when_there_are_no_modes_at_all() {
        let topo = Topology {
            paths: Vec::new(),
            modes: Vec::new(),
        };
        assert_eq!(topo.source_mode(&path_at(0)), None);
    }

    // --- decoding the fixed-size buffers the Win32 calls fill in ---

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn wide_to_string_stops_at_the_terminator() {
        let mut buf = wide("DISPLAY1");
        buf.push(0);
        buf.extend(wide("leftover rubbish"));
        assert_eq!(wide_to_string(&buf), "DISPLAY1");
    }

    #[test]
    fn wide_to_string_accepts_a_buffer_with_no_terminator() {
        assert_eq!(wide_to_string(&wide("DISPLAY1")), "DISPLAY1");
    }

    #[test]
    fn wide_to_string_handles_empty_and_immediately_terminated_buffers() {
        assert_eq!(wide_to_string(&[]), "");
        assert_eq!(wide_to_string(&[0]), "");
        assert_eq!(wide_to_string(&[0, 0, 0]), "");
    }

    #[test]
    fn wide_to_string_trims_padding() {
        assert_eq!(wide_to_string(&wide("  LG ULTRAGEAR  ")), "LG ULTRAGEAR");
    }

    /// An unpaired surrogate must come back lossily rather than panicking: this
    /// decodes whatever bytes the driver put in the buffer.
    #[test]
    fn wide_to_string_survives_invalid_utf16() {
        let decoded = wide_to_string(&[0xD800, 0x0041]);
        assert!(decoded.contains('A'), "{decoded:?}");
    }

    #[test]
    fn non_empty_maps_blank_to_none() {
        assert_eq!(non_empty(String::new()), None);
        assert_eq!(non_empty("x".to_string()), Some("x".to_string()));
    }

    // --- EDID id decoding ---

    /// The four monitors this was built against. The manufacturer ids are the
    /// little-endian u16 Windows reports for the big-endian pair EDID stores —
    /// Dell's `10 AC` arrives as `0xAC10` — so this pins the byte swap as well
    /// as the 5-bit unpacking.
    #[test]
    fn format_edid_id_decodes_real_monitors() {
        assert_eq!(format_edid_id(0x6936, 0x3DD2), "MSI3DD2");
        assert_eq!(format_edid_id(0x6D1E, 0x5BD3), "GSM5BD3");
        assert_eq!(format_edid_id(0xAC10, 0xA0BC), "DELA0BC");
        assert_eq!(format_edid_id(0x0C41, 0x01EA), "PHL01EA");
    }

    /// Dropping the swap would decode the same bytes as something else
    /// entirely; this fails if anyone decides the swap looks redundant.
    #[test]
    fn format_edid_id_is_sensitive_to_the_byte_swap() {
        assert_ne!(
            format_edid_id(0xAC10, 0xA0BC),
            format_edid_id(0x10AC, 0xA0BC)
        );
    }

    #[test]
    fn format_edid_id_pads_the_product_code_to_four_digits() {
        assert_eq!(format_edid_id(0x0C41, 0x0001), "PHL0001");
    }

    /// Five bits hold 0..=31, but only 1..=26 name a letter. Anything else is
    /// a manufacturer id that was never filled in.
    #[test]
    fn format_edid_id_marks_letters_that_are_out_of_range() {
        assert_eq!(format_edid_id(0x0000, 0x0000), "???0000");
        // 27, 27, 27 — in range for the field, outside the alphabet.
        let packed: u16 = (27 << 10) | (27 << 5) | 27;
        assert_eq!(format_edid_id(packed.swap_bytes(), 0x1234), "???1234");
    }

    #[test]
    fn output_technology_names_the_connectors_in_use_here() {
        use windows::Win32::Devices::Display as d;
        assert_eq!(
            output_technology_name(d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI.0),
            "HDMI"
        );
        assert_eq!(
            output_technology_name(d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL.0),
            "DisplayPort"
        );
        // 0x80000000, which is also i32::MIN — so this is the value an
        // "obviously out of range" probe would accidentally land on.
        assert_eq!(
            output_technology_name(d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL.0),
            "Internal"
        );
        assert_eq!(
            output_technology_name(d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_OTHER.0),
            "other"
        );
        // A real connector type the match does not name.
        assert_eq!(
            output_technology_name(d::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_SVIDEO.0),
            "other"
        );
    }
}
