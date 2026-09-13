//! `list` — enumerate every connected output and its stable identity.
//!
//! Read-only. This is the command you run to find out what to put in the config
//! file, and the one that proves the core assumption: a target id stays put when
//! its display is switched off, where the EDID-based identifiers go blank.

use anyhow::Result;

use crate::config::Config;
use crate::display::{ccd, Monitor};

pub fn run(monitors: &[Monitor], config: &Config, active_only: bool, json: bool) -> Result<()> {
    let shown: Vec<&Monitor> = monitors
        .iter()
        .filter(|m| !active_only || m.active)
        .collect();

    // Adapter paths are long and identical across every row on a single-GPU
    // machine. Number them and print a legend instead of repeating them.
    let mut adapters: Vec<&str> = Vec::new();
    for m in &shown {
        if !adapters.contains(&m.key.adapter.as_str()) {
            adapters.push(&m.key.adapter);
        }
    }
    let gpu_of = |adapter: &str| {
        adapters
            .iter()
            .position(|a| *a == adapter)
            .map(|i| format!("gpu{i}"))
            .unwrap_or_else(|| "?".into())
    };

    if json {
        let rows: Vec<_> = shown
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": config.name_for(&m.key),
                    "adapter": m.key.adapter,
                    "target_id": m.key.target_id,
                    // Transient, this boot only — shown so you can confirm it
                    // changes across a reboot while target_id does not.
                    "adapter_luid": format!(
                        "0x{:08X}-0x{:08X}",
                        m.adapter_luid.HighPart, m.adapter_luid.LowPart
                    ),
                    "source_id": m.source_id,
                    "active": m.active,
                    "available": m.available,
                    "connector": ccd::output_technology_name(m.name.output_technology),
                    "friendly": m.name.friendly,
                    "edid": m.name.edid,
                    "monitor_device_path": m.name.device_path,
                    "gdi_name": m.gdi_name,
                    "width": m.mode.map(|s| s.width),
                    "height": m.mode.map(|s| s.height),
                    "x": m.mode.map(|s| s.x),
                    "y": m.mode.map(|s| s.y),
                    "refresh_hz": m.refresh_hz,
                })
            })
            .collect();
        crate::log!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    let rows: Vec<[String; 8]> = shown
        .iter()
        .map(|m| {
            let mode = match m.mode {
                Some(s) => match m.refresh_hz {
                    Some(hz) => format!("{}x{} @ {}", s.width, s.height, format_hz(hz)),
                    None => format!("{}x{}", s.width, s.height),
                },
                None => "-".into(),
            };
            let position = match m.mode {
                Some(s) => format!("{},{}", s.x, s.y),
                None => "-".into(),
            };
            [
                config.name_for(&m.key).unwrap_or("-").to_string(),
                if m.active { "active" } else { "inactive" }.to_string(),
                gpu_of(&m.key.adapter),
                m.key.target_id.to_string(),
                ccd::output_technology_name(m.name.output_technology).to_string(),
                mode,
                position,
                m.label(),
            ]
        })
        .collect();

    let headers = [
        "NAME", "STATE", "GPU", "TARGET", "CONN", "MODE", "POSITION", "MONITOR",
    ];
    print_table(&headers, &rows);

    if !adapters.is_empty() {
        crate::log!();
        for (i, a) in adapters.iter().enumerate() {
            crate::log!("gpu{i}  {a}");
        }
    }

    if config.targets.is_empty() {
        crate::log!(
            "\nNo targets named yet. Arrange your displays in Settings, then run\n  \
             monitor-switcher save-profile <name>\nto record this layout."
        );
    }
    Ok(())
}

/// Refresh rates come back as exact rationals, and the difference between a
/// 59.951Hz panel and a 60Hz one is real — don't round it away. Integers still
/// print as integers.
fn format_hz(hz: f64) -> String {
    if (hz - hz.round()).abs() < 0.0005 {
        return format!("{hz:.0}Hz");
    }
    let s = format!("{hz:.3}");
    format!("{}Hz", s.trim_end_matches('0').trim_end_matches('.'))
}

fn print_table(headers: &[&str; 8], rows: &[[String; 8]]) {
    for line in render_table(headers, rows) {
        crate::log!("{line}");
    }
}

/// The table as lines, header first.
///
/// Separate from printing it so the column arithmetic can be checked directly
/// rather than by capturing stdout.
fn render_table(headers: &[&str; 8], rows: &[[String; 8]]) -> Vec<String> {
    // Column widths in characters, not bytes: a monitor name is whatever the
    // panel calls itself, and counting bytes would over-pad anything non-ASCII.
    let mut width = headers.map(|h| h.chars().count());
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            width[i] = width[i].max(cell.chars().count());
        }
    }
    let line = |cells: &[String; 8]| {
        let mut s = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                s.push_str(cell); // don't pad the last column
            } else {
                s.push_str(&format!("{:<w$}  ", cell, w = width[i]));
            }
        }
        s.trim_end().to_string()
    };

    let mut out = vec![line(&headers.map(String::from))];
    out.extend(rows.iter().map(line));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four rates on the desk this was written for. The point of the
    /// function is that 59.951 and 59.95 are different panels and must not
    /// both round to 60.
    const HEADERS: [&str; 8] = [
        "NAME", "STATE", "GPU", "TARGET", "CONN", "MODE", "POSITION", "MONITOR",
    ];

    fn cells(values: [&str; 8]) -> [String; 8] {
        values.map(String::from)
    }

    /// Every column starts at the same offset on every line, header included.
    #[test]
    fn render_table_aligns_each_column_across_every_line() {
        let rows = [
            cells([
                "main",
                "active",
                "gpu0",
                "37121",
                "DisplayPort",
                "3840x2160",
                "0,0",
                "MSI",
            ]),
            cells([
                "sidemon", "inactive", "gpu0", "37124", "HDMI", "-", "-", "LG",
            ]),
        ];
        let out = render_table(&HEADERS, &rows);
        assert_eq!(out.len(), 3);

        let sources = [cells(HEADERS), rows[0].clone(), rows[1].clone()];
        // Widest cell per column, plus the two-space separator.
        let widths = [7usize, 8, 4, 6, 11, 9, 8];
        let mut offset = 0;
        for (col, width) in widths.iter().enumerate() {
            for (line, source) in out.iter().zip(&sources) {
                assert!(
                    line[offset..].starts_with(&source[col]),
                    "column {col} of {line:?} does not begin at {offset}"
                );
            }
            offset += width + 2;
        }
    }

    /// The last column is not padded, so no row carries invisible trailing
    /// whitespace into a terminal or a copy-paste.
    #[test]
    fn render_table_leaves_no_trailing_whitespace() {
        let rows = [cells(["a", "b", "c", "d", "e", "f", "g", "h"])];
        for line in render_table(&HEADERS, &rows) {
            assert_eq!(line, line.trim_end(), "trailing space in {line:?}");
        }
    }

    #[test]
    fn render_table_of_no_rows_is_just_the_header() {
        let out = render_table(&HEADERS, &[]);
        assert_eq!(
            out,
            ["NAME  STATE  GPU  TARGET  CONN  MODE  POSITION  MONITOR"]
        );
    }

    /// Widths are counted in characters. Measuring bytes would pad a column
    /// holding any non-ASCII name one place too far for every row in it.
    #[test]
    fn render_table_measures_characters_not_bytes() {
        let headers = ["N", "S", "G", "T", "C", "M", "P", "MONITOR"];
        let rows = [
            // Seven characters, eight bytes.
            cells(["ölandet", "b", "c", "d", "e", "f", "g", "h"]),
            cells(["1234567", "b", "c", "d", "e", "f", "g", "h"]),
        ];
        let out = render_table(&headers, &rows);
        for line in &out[1..] {
            let chars: Vec<char> = line.chars().collect();
            assert_eq!(
                chars[9], 'b',
                "second column should start at character 9 of {line:?}"
            );
        }
    }

    #[test]
    fn format_hz_keeps_the_rates_apart() {
        assert_eq!(format_hz(60.0), "60Hz");
        assert_eq!(format_hz(59.951), "59.951Hz");
        assert_eq!(format_hz(59.95), "59.95Hz");
        assert_eq!(format_hz(239.99), "239.99Hz");
    }

    /// Rationals that are integers in all but the last decimal place — which is
    /// how the API reports a plain 60Hz — print as integers.
    #[test]
    fn format_hz_treats_a_near_integer_as_an_integer() {
        assert_eq!(format_hz(59.9999), "60Hz");
        assert_eq!(format_hz(60.0001), "60Hz");
        assert_eq!(format_hz(144.0), "144Hz");
    }

    #[test]
    fn format_hz_drops_only_trailing_zeros() {
        assert_eq!(format_hz(120.5), "120.5Hz");
        assert_eq!(format_hz(100.25), "100.25Hz");
    }
}
