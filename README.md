# monitor-switcher

[![CI](https://github.com/marhag87/monitor-switcher/actions/workflows/ci.yml/badge.svg)](https://github.com/marhag87/monitor-switcher/actions/workflows/ci.yml)

**Swap which monitors are switched on, from a single keypress.**

A graphics card can drive more displays than it can drive *at once*. Four
monitors on the desk, three outputs' worth of bandwidth, and two of them take
turns — so switching between them ought to be one hotkey. It isn't, because
turning a display back on is much harder than turning it off.

Every lightweight monitor tool drives the legacy GDI display API, and that API
can disable a display perfectly well but cannot reliably re-enable one.
`MultiMonitorTool /enable <MonitorID>` silently does nothing. The one form that
sometimes works addresses displays as `\\.\DISPLAY2`, a slot number Windows
reassigns as displays come and go, so it can't be written down in advance. And
once a display goes inactive, that class of tool loses its EDID-based
identifiers entirely — monitor ID, short monitor ID and serial number all come
back blank, so there is nothing stable left to name it by.

Meanwhile Windows' own Settings app re-enables either display correctly every
single time. This tool does what Settings does: it uses the modern CCD
(Connecting and Configuring Displays) API, which can see outputs that are
currently switched off, and addresses them by an identifier that survives being
switched off.

> [!WARNING]
> **AI-generated code.** This project was written largely by an AI assistant.
> It reconfigures display topology through the Win32 CCD API, drives monitor
> settings over DDC/CI, and with the `cec` feature powers a television on and
> off over the HDMI cable. The tray daemon additionally takes global hotkeys
> and, if you ask it to, adds itself to the per-user `Run` key so it starts at
> sign-in. It has been verified on exactly one machine — a
> single RTX 4080 SUPER driving four displays. Review it yourself before
> running it. It is provided "as is",
> without warranty of any kind (see [LICENSE](LICENSE)); the author accepts no
> responsibility or liability for any damage, data loss, or unexpected
> behaviour resulting from its use.

## How it works

Three decisions, each one the reason something else stops being a problem:

| Decision | Why |
|---|---|
| Query with `QDC_ALL_PATHS`, not `QDC_ONLY_ACTIVE_PATHS` | Returns every physically connected output, including ones that are switched off. A connector's hot-plug-detect line stays asserted whether or not anything is being driven over it, so a disabled display is still enumerable — this is what the GDI tools cannot see. |
| Identify outputs by `(adapter device path, target id)` | The target id is the per-connector identifier Windows uses internally; it is the `UID37120` you can see inside a monitor's device instance path. Unlike EDID, it is still there when the display is off. |
| Apply with `SDC_TOPOLOGY_SUPPLIED` | Says *which* outputs should be on and lets Windows supply the resolutions, refresh rates and positions from its own database of past layouts. Nothing about your arrangement is duplicated here, so nothing here can go stale. |

Note what is deliberately **not** stored: the adapter `LUID`. A LUID is unique
only until the next reboot — Windows regenerates it on restart and on any
driver stop/start — so a config file containing one would quietly start
pointing at nothing. It is resolved fresh on every run from the adapter's PCI
device path, which is stable.

This is not theoretical. Across one reboot on the machine this was built for,
the adapter LUID moved from `0x00000000-0x00010A5D` to `0x00000000-0x0000E852`
while every target id stayed exactly where it was. A config keyed on the LUID
would have stopped resolving at that point, silently.

## Install

Build it:

```
cargo build --release
```

That produces two self-contained executables in `target\release`, about 1.2 MB
each. Nothing needs installing; put them wherever you like.

| | |
|---|---|
| `monitor-switcher.exe` | the command line, below |
| `monitor-switcher-tray.exe` | a notification-area daemon that runs those same commands from a [global hotkey](#a-hotkey-that-actually-fires) |

They are separate binaries because they need opposite Windows subsystems: one
has to be able to print to a console, and the other must not own one. The code
is the same — the daemon runs a hotkey's action by handing the command line the
words you would otherwise have typed.

Rust stable is pinned via `rust-toolchain.toml`, so a machine defaulting to
nightly still builds this with stable.

No administrator rights are needed, for building or running.

Check it:

```
cargo test
```

The tests need no displays and no GPU. The code that interprets a display
topology is kept apart from the code that asks Windows for one, so it can be run
against a topology built in the test. What genuinely talks to hardware —
`SetDisplayConfig`, the DDC/CI calls — is left alone, because standing a mock in
front of it would only test the mock. [CI](.github/workflows/ci.yml) runs
formatting, clippy, the tests and a release build on Windows, and also lints the
`cec` feature, which every other step compiles out.

The default build has no dependency on libCEC and no TV power control; see
[TV power over HDMI-CEC](#tv-power-over-hdmi-cec) to turn that on.

## Use

Start by seeing what you have:

```
monitor-switcher list
```

```
NAME     STATE     GPU   TARGET  CONN         MODE                  POSITION   MONITOR
main     active    gpu0  37121   DisplayPort  3840x2160 @ 239.99Hz  0,0        MPG321UX OLED [MSI3DD2]
dell     active    gpu0  37123   HDMI         1920x1200 @ 59.95Hz   3840,401   DELL U2415 [DELA0BC]
sidemon  active    gpu0  37124   DisplayPort  2560x1440 @ 59.951Hz  -2560,167  LG ULTRAGEAR [GSM5BD3]
tv       inactive  gpu0  37120   HDMI         -                     -          Philips FTV [PHL01EA]
```

Arrange your displays the way you want them in Settings, then record that
arrangement under a name:

```
monitor-switcher save-profile 3monitor
```

Do the same for the other arrangement, and you have two profiles to alternate
between:

```
monitor-switcher apply-profile tv
monitor-switcher switch
```

`switch` compares what's currently on against the first of the two profiles: if
that is what you're looking at you get the second, and otherwise the first. So
it alternates, and from any *other* arrangement it goes to the first profile
rather than refusing — which is what makes a hotkey a way back out of an odd
state. That's the one to bind to a key.

| Command | |
|---|---|
| `list [--active] [--json]` | Every connected output and its stable identity |
| `save-profile <name> [--force]` | Record the currently active outputs under a name |
| `apply-profile <name> [--dry-run]` | Make that profile's outputs the active ones |
| `switch [<a> <b>] [--dry-run]` | Alternate between two profiles |
| `vcp get\|set\|switch\|caps` | A monitor's own settings over [DDC/CI](#monitor-settings-over-ddcci) |
| `cec status\|on\|off` | TV power over [HDMI-CEC](#tv-power-over-hdmi-cec), with the `cec` feature |

`--dry-run` validates a change through `SDC_VALIDATE` without applying it.
Applying a profile that is already active is a no-op: it returns in well under a
second without calling `SetDisplayConfig`, so no display flickers.

Everything reports a real error code and a non-zero exit status on failure —
silent failure is the problem this tool exists to solve, so nothing here fails
quietly.

## Configuration

`%LOCALAPPDATA%\monitor-switcher\config.json`, or pass `--config <path>`. Note
that nothing creates it for you on first run and it is not read from next to the
exe — `save-profile` writes it, or copy [`config.json.example`](config.json.example)
into place and edit it:

```
mkdir %LOCALAPPDATA%\monitor-switcher
copy config.json.example %LOCALAPPDATA%\monitor-switcher\config.json
```

The generated target names come from the monitors themselves, and shorter ones
are usually nicer:

```jsonc
{
  "targets": {
    "main": {
      "adapter": "\\\\?\\PCI#VEN_10DE&DEV_2702&...",
      "target_id": 37121,
      "edid": "MSI3DD2",
      "friendly": "MPG321UX OLED"
    }
    // ... sidemon, dell, tv
  },
  "profiles": {
    "3monitor": ["main", "sidemon", "dell"],
    "tv":       ["main", "sidemon", "tv"]
  },
  "switch": ["3monitor", "tv"]
}
```

A profile is just the set of outputs that should be on. Resolutions and
positions are Windows' business, not this file's. It really is a set: naming an
output twice — directly, or through two target names that point at the same
output — means the same as naming it once.

`edid` and `friendly` are recorded for your benefit and for addressing monitors
over DDC/CI; neither is used to identify anything for topology purposes.

`hotkeys` is read only by the tray daemon, and maps a key combination to a
command written exactly as you would type it after `monitor-switcher`:

```jsonc
"hotkeys": {
  "ctrl+alt+shift+d": "switch",
  "ctrl+alt+shift+s": "vcp switch main 60 15 18"
}
```

Modifiers are `ctrl`, `shift`, `alt` and `win`, in any order and any case. A
combination with no modifier is refused: `RegisterHotKey` would take that key
system-wide, leaving it unusable everywhere else. Each command is checked with
the same parser the command line uses, so a typo is reported when the daemon
starts rather than becoming a key that does nothing.

## Monitor settings over DDC/CI

DDC/CI is the protocol behind the buttons on the front of a monitor — input
source, brightness, contrast, and on many panels power — reachable over the
video cable. Features are addressed by **VCP code**, a byte from the MCCS
standard: `60` is input select, `10` brightness, `D6` power.

```
monitor-switcher vcp caps <target>                    # what does it support?
monitor-switcher vcp get <target> <code>
monitor-switcher vcp set <target> <code> <value>
monitor-switcher vcp switch <target> <code> <v1> <v2> [...]
```

Following ControlMyMonitor, **codes are hex and values are decimal**. So
`ControlMyMonitor.exe /SwitchValue MSI3DD2 60 15 18` becomes:

```
monitor-switcher vcp switch main 60 15 18
```

`switch` reads the current value, moves to the next one you listed and wraps —
stateless, so it can't get out of step with the monitor. If the display is on
something not in your list, it jumps to the first entry.

Start with `caps`, because a monitor tells you exactly what it accepts:

```
vcp(02 04 05 ... 10 12 14(01 04 ...) ... D6(04) ... 60(11 12 0F 10) ...)
```

That is the MSI: input select accepts `0F`/`10`/`11`/`12` (15, 16, 17, 18 in
decimal — DisplayPort-1, DisplayPort-2, HDMI-1, HDMI-2), brightness is
supported, and power offers only `04`, "off", with no matching "on".

Targets are named as in your config, or by EDID id so old ControlMyMonitor
command lines port across unchanged. Only **active** displays can be reached —
DDC/CI rides on the video link, so a display whose output is switched off has
no channel and says so.

Not every display implements it. Monitors generally do, televisions generally
don't — the Philips TV here answers nothing at all, and reports
`ERROR_GRAPHICS_I2C_ERROR_TRANSMITTING_DATA` when asked. That's what
[CEC](#tv-power-over-hdmi-cec) is for.

## TV power over HDMI-CEC

Switching a GPU output on does not switch the *display* on. For a monitor you
would do that over DDC/CI, but televisions generally don't implement it — the
Philips set this was built for answers nothing over DDC, not power, not
brightness, not even input select. Televisions do HDMI-CEC instead.

CEC needs hardware, because GPUs don't carry it: a Pulse-Eight USB-CEC adapter,
inline between the graphics card and the TV, with USB to the PC. With one
fitted, `switch` can wake the TV and take over its input on the way in, and put
it back to standby on the way out.

It is off by default and opt-in at build time:

```
cargo build --release --features cec
```

That needs **libCEC x64** installed ([releases][libcec]; the installer defaults
to the 32-bit build, which cannot link into a 64-bit binary). The Windows
package ships `cec.dll` and headers but no import library, so `build.rs`
generates one from the DLL's exports and copies the DLL next to the executable
— nothing derived from libCEC is committed here, and it can't drift out of step
with what's installed.

Then give the display a `cec` block:

```jsonc
"tv": {
  "adapter": "...", "target_id": 37120, "edid": "PHL01EA",
  "cec": {
    "hdmi_port": 1,          // which input on the TV the adapter feeds
    "power_on": true,        // wake it when a profile activates it
    "standby": true,         // sleep it when a profile deactivates it
    "activate_source": true  // take over the input, so it shows this PC
  }
}
```

`hdmi_port` matters more than it looks: it sets the physical address claimed on
the CEC bus, which is what makes taking over the input work. Get it wrong and
power still works while `activate_source` may not.

`monitor-switcher cec status|on|off` drives power directly without touching
topology — the quickest way to tell whether the adapter is at fault when a
`switch` doesn't do what you expected.

Power control is always best-effort: if the adapter is missing or the TV
ignores a command, `switch` says so and still changes the display topology. A
missing adapter must never stop your monitors switching.

> [!IMPORTANT]
> **libCEC is GPL-2.0-or-later** (or commercial, from Pulse-Eight), so a binary
> built with `--features cec` is a combined work and can only be distributed
> under the GPL — as can `cec.dll` itself. This project's own source stays MIT,
> and the default build links nothing but permissively licensed crates. If you
> distribute binaries, ship default-feature ones and let people build the CEC
> variant themselves.

[libcec]: https://github.com/Pulse-Eight/libcec/releases/latest

## A hotkey that actually fires

The obvious way to bind this — a shortcut with a Shortcut key in its properties
— is unreliable, and not in a way you can configure around. Explorer owns those
bindings, so they die when it restarts; the `.lnk` has to stay in the Start menu
or on the Desktop or the binding quietly stops working; and there is a lag after
signing in during which nothing happens. Worst of all, a binding that does not
fire never says why.

`monitor-switcher-tray.exe` uses `RegisterHotKey`, the actual Win32 mechanism
for a global hotkey, which **fails loudly at registration** when another program
already owns a combination instead of by silently never firing:

```
hotkey problem: ctrl+shift+s: Windows would not register it
(error 1409: Hot key is already registered.). Another program probably has it.
```

Put a `hotkeys` block in the config and run it. The icon in the notification
area is the only sign it is there; right-click for the menu:

| | |
|---|---|
| The active profile | at the top, refreshed whenever the display configuration changes — including changes you make in Settings |
| Each hotkey | listed with its combination, and clickable, so the menu still works when a combination does not |
| Open config file / Open log file | the daemon has no console, so the log is where a failed action explains itself |
| Reload config | rebuild the hotkeys after an edit, without restarting |
| Start automatically at sign-in | writes the usual `HKCU` `Run` value; no administrator rights, same as everything else here |
| Exit | |

The icon says which state it is in by shape as well as colour: filled for ready,
a bar while an action is running, a cross when a hotkey could not be registered
or the last action failed. Actions run on their own thread, so the seconds a TV
takes to wake never freeze the menu, and a second press while one is still
running is ignored rather than queued.

Only one daemon runs at a time. A second would fail to register the same
hotkeys, so it refuses to start and says so.

## Known caveats

- **Windows' remembered layout can be wrong.** This tool asks Windows for the
  layout rather than storing its own, which means it inherits whatever Windows
  has saved. On the machine this was built for, the TV's saved layout was
  23.976 Hz in the wrong position. The fix is a one-off: activate that display,
  correct it in Settings, and the database is corrected for good. Note the
  failure mode — a *wrong layout restored*, never a failed switch.
- **A TV will not wake while it is still going to sleep.** A CEC power-on sent
  during the on-to-standby transition is silently dropped, so switching away and
  straight back would otherwise leave the TV dark. The tool waits out the
  transition (up to 12s) before asking, and then waits for the set to confirm it
  is on (up to 15s) — which is why a switch *towards* the TV can take a couple of
  seconds longer than one away from it.
- **Off is not the same as unplugged.** A display in standby keeps its
  hot-plug-detect line asserted, so it stays enumerable and switchable; this was
  confirmed with the TV physically powered off. A display unplugged at the cable
  or switched off at the wall may not, in which case applying a profile that
  names it fails with a message listing what *is* connected.
- **The output ceiling is real and reported as `ERROR_GEN_FAILURE`.** Asking for
  more simultaneous outputs than the card can drive fails with a code whose
  system text ("a device attached to the system is not functioning") is
  unhelpful, so the message spells out the likely cause. The ceiling can also
  bite one step earlier, as "no free GPU source", when there is no way to give
  every wanted output a source of its own to be driven from.
- **Path priority is not preserved.** `SDC_ALLOW_PATH_ORDER_CHANGES` lets the
  database match on the *set* of outputs rather than their order, which is what
  makes the lookup reliable. If path priority matters to you, it isn't being
  controlled here.
- **One machine.** Verified on a single-GPU desktop with four displays, three
  active at a time. The `(adapter device path, target id)` design should
  generalise to multiple GPUs, but that is untested. A green CI badge does not
  change this: that runner has no displays, so it proves the code builds and its
  logic holds, not that any of it drives real hardware correctly.

## Not in scope

Nothing further is planned. The three protocols a display can be spoken to on
are all covered, and it is worth knowing which does what, since it isn't
obvious:

| | Reaches | Used for |
|---|---|---|
| CCD | The GPU | Which outputs are active, and the desktop layout |
| DDC/CI | Monitors, over the video cable | The monitor's own settings — input, brightness, power |
| CEC | Televisions, over HDMI | Power and input, where DDC/CI isn't implemented |

On this desk the LG answers DDC/CI fully including power (`VCP D6`), while the
Philips TV answers none of it and speaks only CEC. That split — monitors do
DDC/CI, televisions do CEC — is the normal one rather than a fault.
