//! Win32 error reporting.
//!
//! The CCD entry points return `WIN32_ERROR` codes rather than `HRESULT`s, and
//! silent failure is the exact problem this tool exists to solve. Every failure
//! path here reports the symbolic name, the numeric code, and the system's own
//! message text.

use windows::core::PWSTR;
use windows::Win32::System::Diagnostics::Debug::{
    FormatMessageW, FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS,
};

/// Symbolic name for the codes `SetDisplayConfig` documents, plus the handful
/// the query path can return.
pub fn name(code: u32) -> Option<&'static str> {
    Some(match code {
        0 => "ERROR_SUCCESS",
        5 => "ERROR_ACCESS_DENIED",
        13 => "ERROR_INVALID_DATA",
        31 => "ERROR_GEN_FAILURE",
        50 => "ERROR_NOT_SUPPORTED",
        87 => "ERROR_INVALID_PARAMETER",
        122 => "ERROR_INSUFFICIENT_BUFFER",
        1004 => "ERROR_INVALID_FLAGS",
        1610 => "ERROR_BAD_CONFIGURATION",
        _ => return None,
    })
}

/// The system's message text for an error code, if it has one.
fn message(code: u32) -> Option<String> {
    let mut buf = [0u16; 512];
    // SAFETY: buf is a valid writable slice; FORMAT_MESSAGE_FROM_SYSTEM with
    // IGNORE_INSERTS takes no argument array, so passing None is correct.
    let len = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            None,
            code,
            0,
            PWSTR(buf.as_mut_ptr()),
            buf.len() as u32,
            None,
        )
    };
    if len == 0 {
        return None;
    }
    let text = String::from_utf16_lossy(&buf[..len as usize]);
    let text = text.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// An extra line of guidance for codes where the raw text is unhelpful in this
/// tool's specific context.
fn hint(code: u32) -> Option<&'static str> {
    Some(match code {
        1610 => "The requested topology has no entry in the display database.",
        31 => {
            "The driver refused this configuration. Usually this means the GPU cannot drive \
             this many outputs at once, or not all of them at these resolutions."
        }
        5 => {
            "SetDisplayConfig requires access to the console session; it fails over Remote \
             Desktop and from a service."
        }
        50 => "This requires a WDDM display driver.",
        _ => return None,
    })
}

/// Render an error code for display: `ERROR_BAD_CONFIGURATION (1610): <text>`.
pub fn describe(code: u32) -> String {
    let mut out = match name(code) {
        Some(n) => format!("{n} ({code})"),
        None => format!("error {code}"),
    };
    if let Some(m) = message(code) {
        out.push_str(": ");
        out.push_str(&m);
    }
    if let Some(h) = hint(code) {
        out.push_str("\n  ");
        out.push_str(h);
    }
    out
}
