//! DDC/CI — talking to a monitor's own controls over the video cable.
//!
//! This is the protocol behind the buttons on the front of a monitor: input
//! source, brightness, contrast, and on many panels power. Features are
//! addressed by VCP code, a byte defined by the MCCS standard — `0x60` is input
//! select, `0x10` brightness, `0xD6` power mode.
//!
//! Not every display implements it. Computer monitors generally do;
//! televisions generally do not, and use [CEC](crate::cec) instead. On this
//! desk the LG answers everything including power while the Philips TV answers
//! nothing at all, which is the normal split rather than a fault.
//!
//! Only active displays can be reached: DDC/CI rides on the video link, so a
//! display whose output is switched off has no channel to talk over.

use std::mem::size_of;

use anyhow::{bail, Context, Result};
use windows::Win32::Devices::Display::{
    CapabilitiesRequestAndCapabilitiesReply, DestroyPhysicalMonitors, GetCapabilitiesStringLength,
    GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR,
    GetVCPFeatureAndVCPFeatureReply, SetVCPFeature, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{GetLastError, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};

/// A monitor's reply about one VCP feature.
#[derive(Debug, Clone, Copy)]
pub struct Vcp {
    pub current: u32,
    pub maximum: u32,
}

/// An open DDC/CI channel to one physical display.
pub struct Ddc {
    /// Kept as an array because that is what `DestroyPhysicalMonitors` frees.
    monitors: Vec<PHYSICAL_MONITOR>,
}

impl Ddc {
    /// Open the display currently driven by a GDI device, e.g. `\\.\DISPLAY1`.
    ///
    /// That name is transient — Windows reassigns it as displays come and go —
    /// so it is never stored anywhere. Callers resolve a stable target to its
    /// current GDI name first, and pass the result straight here.
    pub fn open(gdi_device: &str) -> Result<Self> {
        let hmonitor = find_hmonitor(gdi_device)?;

        let mut count = 0u32;
        // SAFETY: hmonitor came from EnumDisplayMonitors; count is a valid out-param.
        unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) }
            .context("counting physical monitors")?;
        if count == 0 {
            bail!("{gdi_device} reports no physical monitors");
        }

        let mut monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
        // SAFETY: the slice is exactly the length the call just asked for.
        unsafe { GetPhysicalMonitorsFromHMONITOR(hmonitor, &mut monitors) }
            .context("opening the monitor's DDC/CI channel")?;

        Ok(Self { monitors })
    }

    fn handle(&self) -> windows::Win32::Foundation::HANDLE {
        self.monitors[0].hPhysicalMonitor
    }

    /// Read a VCP feature's current and maximum value.
    pub fn get(&self, code: u8) -> Result<Vcp> {
        let (mut current, mut maximum) = (0u32, 0u32);
        // SAFETY: handle is live for the lifetime of self; both out-params valid.
        let ok = unsafe {
            GetVCPFeatureAndVCPFeatureReply(
                self.handle(),
                code,
                None,
                &mut current,
                Some(&mut maximum),
            )
        };
        if ok == 0 {
            bail!("{}", unsupported(code));
        }
        Ok(Vcp { current, maximum })
    }

    /// Set a VCP feature.
    pub fn set(&self, code: u8, value: u32) -> Result<()> {
        // SAFETY: handle is live for the lifetime of self.
        let ok = unsafe { SetVCPFeature(self.handle(), code, value) };
        if ok == 0 {
            bail!("{}", unsupported(code));
        }
        Ok(())
    }

    /// The monitor's capabilities string: an ASCII blob listing the VCP codes
    /// it implements, and for non-continuous ones the values it accepts.
    pub fn capabilities(&self) -> Result<String> {
        let mut len = 0u32;
        // SAFETY: handle is live; len is a valid out-param.
        let ok = unsafe { GetCapabilitiesStringLength(self.handle(), &mut len) };
        if ok == 0 || len == 0 {
            bail!("this display does not report a capabilities string");
        }

        let mut buf = vec![0u8; len as usize];
        // SAFETY: buffer is exactly the length the call just reported.
        let ok = unsafe { CapabilitiesRequestAndCapabilitiesReply(self.handle(), &mut buf) };
        if ok == 0 {
            bail!("reading the capabilities string failed");
        }
        // The reply is NUL-terminated ASCII.
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
    }
}

impl Drop for Ddc {
    fn drop(&mut self) {
        // SAFETY: these handles came from GetPhysicalMonitorsFromHMONITOR and
        // are freed exactly once, here.
        let _ = unsafe { DestroyPhysicalMonitors(&self.monitors) };
    }
}

/// Both failure paths look the same from the API's side: a display that does
/// not implement a code, and one that does not speak DDC/CI at all, each just
/// return failure. Say so rather than reporting a bare error number.
fn unsupported(code: u8) -> String {
    // SAFETY: reading the calling thread's last-error value.
    let err = unsafe { GetLastError() };
    format!(
        "the display did not answer VCP 0x{code:02X} \
         (it may not implement this feature, or DDC/CI at all) [{}]",
        crate::winerr::describe(err.0)
    )
}

/// Find the `HMONITOR` whose GDI device name matches.
fn find_hmonitor(gdi_device: &str) -> Result<HMONITOR> {
    let mut found: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only appends to the Vec pointed at by dwdata, which
    // outlives the call.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut found as *mut Vec<HMONITOR> as isize),
        )
    };
    // Our callback always continues, so a false return is a real failure —
    // distinct from enumerating successfully and not finding a match.
    if !ok.as_bool() {
        // SAFETY: reading the calling thread's last-error value.
        let err = unsafe { GetLastError() };
        bail!(
            "enumerating attached displays failed: {}",
            crate::winerr::describe(err.0)
        );
    }

    for hmonitor in found {
        if device_name(hmonitor).as_deref() == Some(gdi_device) {
            return Ok(hmonitor);
        }
    }
    bail!("no attached display is currently driven by {gdi_device}")
}

unsafe extern "system" fn collect(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: data is the &mut Vec set up by find_hmonitor.
    unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) }.push(hmonitor);
    TRUE
}

fn device_name(hmonitor: HMONITOR) -> Option<String> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: cbSize declares the larger MONITORINFOEXW, which is what we pass.
    let ok = unsafe { GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut MONITORINFO) };
    if !ok.as_bool() {
        return None;
    }
    let end = info
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szDevice.len());
    Some(String::from_utf16_lossy(&info.szDevice[..end]))
}
