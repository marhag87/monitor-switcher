//! Parsing a hotkey written the way people write them: `ctrl+shift+s`.
//!
//! `RegisterHotKey` wants a modifier bitmask and a virtual-key code, and
//! nothing in Windows will turn a string into those. This does, and it is the
//! one part of the daemon that is pure enough to test.
//!
//! A combination with no modifier is rejected. `RegisterHotKey` would happily
//! accept it and then swallow that key system-wide, which is a spectacular way
//! to make a machine unusable from a one-word typo in a config file.

use anyhow::{bail, Result};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};

/// A parsed hotkey, ready for `RegisterHotKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub modifiers: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

impl Binding {
    /// How the combination should be written back to a human — in the tray
    /// menu, and in the message when registering it fails.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        for (flag, name) in [
            (MOD_CONTROL, "Ctrl"),
            (MOD_SHIFT, "Shift"),
            (MOD_ALT, "Alt"),
            (MOD_WIN, "Win"),
        ] {
            if self.modifiers.0 & flag.0 != 0 {
                parts.push(name.to_string());
            }
        }
        parts.push(key_name(self.vk));
        parts.join("+")
    }
}

/// Parse `ctrl+shift+s` and the like. Case and spacing are not significant.
pub fn parse(spec: &str) -> Result<Binding> {
    let mut modifiers = 0u32;
    let mut key: Option<u32> = None;

    for part in spec.split('+') {
        let part = part.trim();
        if part.is_empty() {
            bail!("\"{spec}\" has an empty part; write it like ctrl+shift+s");
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= MOD_CONTROL.0,
            "shift" => modifiers |= MOD_SHIFT.0,
            "alt" | "menu" => modifiers |= MOD_ALT.0,
            "win" | "super" | "meta" => modifiers |= MOD_WIN.0,
            name => {
                let vk = key_code(name).ok_or_else(|| {
                    anyhow::anyhow!("\"{part}\" in \"{spec}\" is not a key this understands")
                })?;
                if let Some(first) = key {
                    bail!(
                        "\"{spec}\" names two keys ({} and {}); a hotkey has one key and any number of modifiers",
                        key_name(first),
                        key_name(vk)
                    );
                }
                key = Some(vk);
            }
        }
    }

    let Some(vk) = key else {
        bail!("\"{spec}\" is all modifiers and no key");
    };
    if modifiers == 0 {
        bail!(
            "\"{spec}\" has no modifier. A bare key would be taken system-wide, \
             leaving it unusable everywhere else — write something like ctrl+shift+{}",
            key_name(vk).to_ascii_lowercase()
        );
    }

    Ok(Binding {
        // Without NOREPEAT a held key repeats at the keyboard's rate, which for
        // an action that switches displays would be a stream of them.
        modifiers: HOT_KEY_MODIFIERS(modifiers | MOD_NOREPEAT.0),
        vk,
    })
}

/// Virtual-key code for a key name. Letters and digits are their own ASCII
/// values, which is what the VK table says for those two ranges.
fn key_code(name: &str) -> Option<u32> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphanumeric() {
            return Some(c.to_ascii_uppercase() as u32);
        }
    }
    if let Some(n) = name.strip_prefix('f') {
        if let Ok(n) = n.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x70 + n - 1); // VK_F1..VK_F24
            }
        }
    }
    Some(match name {
        "space" => 0x20,
        "tab" => 0x09,
        "enter" | "return" => 0x0D,
        "esc" | "escape" => 0x1B,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "pause" => 0x13,
        "scrolllock" => 0x91,
        "printscreen" | "prtsc" => 0x2C,
        _ => return None,
    })
}

/// The inverse, for display only.
fn key_name(vk: u32) -> String {
    // Only the codes `key_code` actually produces: letters and digits are their
    // *uppercase* ASCII values. Accepting lowercase here would be wrong rather
    // than lenient — 0x70..=0x87 is VK_F1..VK_F24, which overlaps 'p'..'z', so
    // F4 would come back as "s".
    if let Some(c) = char::from_u32(vk) {
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return c.to_string();
        }
    }
    if (0x70..=0x87).contains(&vk) {
        return format!("F{}", vk - 0x70 + 1);
    }
    match vk {
        0x20 => "Space".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x1B => "Esc".into(),
        0x08 => "Backspace".into(),
        0x2E => "Delete".into(),
        0x2D => "Insert".into(),
        0x24 => "Home".into(),
        0x23 => "End".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x13 => "Pause".into(),
        0x91 => "ScrollLock".into(),
        0x2C => "PrintScreen".into(),
        other => format!("0x{other:02X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(spec: &str) -> u32 {
        // NOREPEAT is always on; mask it out so the tests read as the written
        // combination rather than as an implementation detail.
        parse(spec).unwrap().modifiers.0 & !MOD_NOREPEAT.0
    }

    /// The two combinations actually bound on this desk.
    #[test]
    fn parses_the_real_bindings() {
        let s = parse("ctrl+shift+s").unwrap();
        assert_eq!(s.modifiers.0 & !MOD_NOREPEAT.0, MOD_CONTROL.0 | MOD_SHIFT.0);
        assert_eq!(s.vk, 'S' as u32);

        let d = parse("ctrl+shift+d").unwrap();
        assert_eq!(d.vk, 'D' as u32);
    }

    #[test]
    fn every_modifier_is_understood_with_its_aliases() {
        assert_eq!(mods("ctrl+a"), MOD_CONTROL.0);
        assert_eq!(mods("control+a"), MOD_CONTROL.0);
        assert_eq!(mods("alt+a"), MOD_ALT.0);
        assert_eq!(mods("win+a"), MOD_WIN.0);
        assert_eq!(mods("super+a"), MOD_WIN.0);
        assert_eq!(
            mods("ctrl+alt+shift+win+a"),
            MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0 | MOD_WIN.0
        );
    }

    #[test]
    fn case_and_spacing_do_not_matter() {
        assert_eq!(
            parse("CTRL+SHIFT+S").unwrap(),
            parse("ctrl+shift+s").unwrap()
        );
        assert_eq!(
            parse(" ctrl + shift + s ").unwrap(),
            parse("ctrl+shift+s").unwrap()
        );
    }

    #[test]
    fn understands_letters_digits_and_function_keys() {
        assert_eq!(parse("ctrl+7").unwrap().vk, '7' as u32);
        assert_eq!(parse("ctrl+f1").unwrap().vk, 0x70);
        assert_eq!(parse("ctrl+f24").unwrap().vk, 0x87);
        assert_eq!(parse("ctrl+space").unwrap().vk, 0x20);
        assert_eq!(parse("ctrl+pageup").unwrap().vk, 0x21);
    }

    /// Auto-repeat would turn a held key into a stream of display switches.
    #[test]
    fn always_asks_for_no_repeat() {
        assert!(parse("ctrl+shift+s").unwrap().modifiers.0 & MOD_NOREPEAT.0 != 0);
    }

    /// The dangerous case: `RegisterHotKey` would accept a bare key and then
    /// swallow it everywhere.
    #[test]
    fn refuses_a_key_with_no_modifier() {
        let err = parse("s").unwrap_err().to_string();
        assert!(err.contains("no modifier"), "{err}");
        assert!(parse("f1").is_err());
    }

    #[test]
    fn refuses_the_other_ways_of_writing_nonsense() {
        for bad in [
            "",
            "ctrl",
            "ctrl+shift",
            "ctrl+",
            "ctrl++s",
            "ctrl+nosuchkey",
            "ctrl+f25",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn refuses_two_keys() {
        let err = parse("ctrl+a+b").unwrap_err().to_string();
        assert!(err.contains("two keys"), "{err}");
    }

    #[test]
    fn describes_itself_the_way_it_was_written() {
        assert_eq!(parse("ctrl+shift+s").unwrap().describe(), "Ctrl+Shift+S");
        assert_eq!(parse("alt+f4").unwrap().describe(), "Alt+F4");
        assert_eq!(parse("win+ctrl+left").unwrap().describe(), "Ctrl+Win+Left");
    }

    /// Round-trip: anything `describe` prints must parse back to itself.
    #[test]
    fn description_parses_back_to_the_same_binding() {
        for spec in [
            "ctrl+shift+s",
            "ctrl+shift+d",
            "alt+f4",
            "win+space",
            "ctrl+alt+delete",
            "ctrl+7",
        ] {
            let one = parse(spec).unwrap();
            let two = parse(&one.describe()).unwrap();
            assert_eq!(one, two, "{spec} described as {}", one.describe());
        }
    }
}

/// One configured hotkey, understood: the combination, and the command it runs.
#[derive(Debug, Clone)]
pub struct Action {
    /// The combination as written in the config, for messages.
    pub spec: String,
    pub binding: Binding,
    /// The command as written, for the tray menu.
    pub command: String,
    /// That command split into words, already known to parse.
    pub words: Vec<String>,
}

/// What the configured hotkeys came to.
///
/// Both halves matter. A daemon that refused to start over one bad line would
/// take the working hotkeys down with it; one that started silently would leave
/// a key that does nothing and no clue why. So the good ones are registered and
/// the rest are explained.
#[derive(Debug, Default)]
pub struct Plan {
    pub ready: Vec<Action>,
    pub problems: Vec<String>,
}

/// Check every configured hotkey: the combination, the command, and whether two
/// entries ask for the same keys.
///
/// The command is validated with the CLI's own parser, so a misspelled
/// subcommand is caught here rather than on the first press.
pub fn plan(configured: &std::collections::BTreeMap<String, String>) -> Plan {
    let mut plan = Plan::default();

    for (spec, command) in configured {
        let binding = match parse(spec) {
            Ok(b) => b,
            Err(e) => {
                plan.problems.push(format!("{spec}: {e}"));
                continue;
            }
        };

        // Two spellings of one combination — "ctrl+shift+s" and "Ctrl+Shift+S"
        // are different config keys but the same keystroke, and the second
        // registration would be the one that failed.
        if let Some(other) = plan.ready.iter().find(|a| a.binding == binding) {
            plan.problems.push(format!(
                "{spec}: the same keys as \"{}\", which is already bound to \"{}\"",
                other.spec, other.command
            ));
            continue;
        }

        let words = crate::app::split_words(command);
        if words.is_empty() {
            plan.problems.push(format!("{spec}: no command to run"));
            continue;
        }
        if let Err(e) = crate::app::check_words(&words) {
            plan.problems.push(format!("{spec}: {e}"));
            continue;
        }

        plan.ready.push(Action {
            spec: spec.clone(),
            binding,
            command: command.clone(),
            words,
        });
    }
    plan
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn configured(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The pair actually bound on this desk.
    #[test]
    fn accepts_the_real_configuration() {
        let plan = plan(&configured(&[
            ("ctrl+shift+d", "switch"),
            ("ctrl+shift+s", "vcp switch main 60 15 18"),
        ]));
        assert!(plan.problems.is_empty(), "{:?}", plan.problems);
        assert_eq!(plan.ready.len(), 2);
        let specs: Vec<&str> = plan.ready.iter().map(|a| a.spec.as_str()).collect();
        assert_eq!(specs, ["ctrl+shift+d", "ctrl+shift+s"]);
    }

    /// One bad line must not cost the good ones.
    #[test]
    fn keeps_the_working_hotkeys_and_explains_the_rest() {
        let plan = plan(&configured(&[
            ("ctrl+shift+d", "switch"),
            ("ctrl+shift+nope", "switch"),
        ]));
        assert_eq!(plan.ready.len(), 1);
        assert_eq!(plan.ready[0].spec, "ctrl+shift+d");
        assert_eq!(plan.problems.len(), 1);
        assert!(plan.problems[0].contains("nope"), "{:?}", plan.problems);
    }

    /// A command that would have failed on the command line fails at startup
    /// instead of becoming a key that quietly does nothing.
    #[test]
    fn rejects_a_command_the_cli_would_reject() {
        let plan = plan(&configured(&[("ctrl+shift+d", "swtich")]));
        assert!(plan.ready.is_empty());
        assert_eq!(plan.problems.len(), 1);
    }

    #[test]
    fn rejects_an_empty_command() {
        let plan = plan(&configured(&[("ctrl+shift+d", "   ")]));
        assert!(plan.ready.is_empty());
        assert!(
            plan.problems[0].contains("no command"),
            "{:?}",
            plan.problems
        );
    }

    /// Different spellings, same keystroke: the second RegisterHotKey would be
    /// the one to fail, so say so up front.
    #[test]
    fn notices_two_entries_asking_for_the_same_keys() {
        let plan = plan(&configured(&[
            ("ctrl+shift+d", "switch"),
            ("CTRL+SHIFT+D", "apply-profile tv"),
        ]));
        assert_eq!(plan.ready.len(), 1);
        assert_eq!(plan.problems.len(), 1);
        assert!(
            plan.problems[0].contains("same keys"),
            "{:?}",
            plan.problems
        );
    }

    #[test]
    fn no_hotkeys_is_not_a_problem() {
        let plan = plan(&BTreeMap::new());
        assert!(plan.ready.is_empty() && plan.problems.is_empty());
    }
}
