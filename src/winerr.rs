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
        // Plain Win32 codes are small and conventionally written in decimal.
        // Anything larger is an HRESULT-shaped value — the DDC/CI calls return
        // these — and is only recognisable, or searchable, in hex.
        None if code > 0xFFFF => format!("error 0x{code:08X}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_covers_the_documented_codes_and_nothing_else() {
        assert_eq!(name(0), Some("ERROR_SUCCESS"));
        assert_eq!(name(5), Some("ERROR_ACCESS_DENIED"));
        assert_eq!(name(1610), Some("ERROR_BAD_CONFIGURATION"));
        assert_eq!(name(1234), None);
    }

    #[test]
    fn describe_leads_with_the_symbolic_name_and_the_number() {
        assert!(
            describe(5).starts_with("ERROR_ACCESS_DENIED (5)"),
            "{}",
            describe(5)
        );
    }

    /// Exercises `FormatMessageW`. The wording is whatever this machine's locale
    /// says, so this checks that a message came back and was attached — not what
    /// it reads.
    #[test]
    fn describe_attaches_the_systems_own_message() {
        let text = describe(5);
        let (head, tail) = text
            .split_once(": ")
            .unwrap_or_else(|| panic!("no message was attached to {text:?}"));
        assert_eq!(head, "ERROR_ACCESS_DENIED (5)");
        assert!(!tail.trim().is_empty(), "{text}");
    }

    #[test]
    fn describe_adds_a_hint_where_the_system_text_is_unhelpful() {
        for (code, expected) in [
            (31u32, "cannot drive"),
            (1610, "display database"),
            (5, "Remote"),
            (50, "WDDM"),
        ] {
            let text = describe(code);
            assert!(text.contains(expected), "{code}: {text}");
        }
    }

    #[test]
    fn describe_leaves_out_a_hint_where_there_is_none() {
        assert!(!describe(0).contains("\n  "), "{}", describe(0));
    }

    /// The DDC/CI calls answer with HRESULT-shaped values. This one is what the
    /// Philips TV really returns when asked for a VCP feature, and it is only
    /// recognisable — or searchable — in hex.
    #[test]
    fn describe_renders_hresult_shaped_codes_in_hex() {
        let text = describe(0xC026_2582);
        assert!(text.starts_with("error 0xC0262582"), "{text}");
    }

    #[test]
    fn describe_renders_small_unknown_codes_in_decimal() {
        assert!(
            describe(1234).starts_with("error 1234"),
            "{}",
            describe(1234)
        );
    }

    /// Every code must render into something, whether or not Windows knows it.
    #[test]
    fn describe_never_returns_nothing() {
        for code in [0u32, 1, 122, 4321, 0x7FFF, 0x1_0000, u32::MAX] {
            assert!(!describe(code).is_empty(), "empty for {code}");
        }
    }
}
