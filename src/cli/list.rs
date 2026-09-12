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
        println!("{}", serde_json::to_string_pretty(&rows)?);
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
        println!();
        for (i, a) in adapters.iter().enumerate() {
            println!("gpu{i}  {a}");
        }
    }

    if config.targets.is_empty() {
        println!(
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
    let mut width = headers.map(|h| h.len());
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
    println!("{}", line(&headers.map(String::from)));
    for row in rows {
        println!("{}", line(row));
    }
}
