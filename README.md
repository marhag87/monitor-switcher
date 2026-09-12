# monitor-switcher

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
> It reconfigures display topology through the Win32 CCD API, and it has been
> verified on exactly one machine — a single RTX 4080 SUPER driving four
> displays. Review it yourself before running it. It is provided "as is",
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

Output is a single self-contained `target\release\monitor-switcher.exe`, about
1 MB. Nothing needs installing; put it wherever you like. Rust stable is pinned
via `rust-toolchain.toml`, so a machine defaulting to nightly still builds this
with stable.

No administrator rights are needed, for building or running.

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

`switch` looks at what's currently on, and applies whichever of the two profiles
isn't it. That's the one to bind to a key.

| Command | |
|---|---|
| `list [--active] [--json]` | Every connected output and its stable identity |
| `save-profile <name> [--force]` | Record the currently active outputs under a name |
| `apply-profile <name> [--dry-run]` | Make that profile's outputs the active ones |
| `switch [<a> <b>] [--dry-run]` | Alternate between two profiles |

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
positions are Windows' business, not this file's.

`edid` and `friendly` are recorded for your benefit and for a possible future
DDC/CI feature; neither is used to identify anything.

## Bind it to a key

Make a shortcut to the exe, put `switch` in its Target after the path, assign a
Shortcut key, and set **Run: Minimized** — this is a console program, so
without that last part you get a console window flashing on every press.

## Known caveats

- **Windows' remembered layout can be wrong.** This tool asks Windows for the
  layout rather than storing its own, which means it inherits whatever Windows
  has saved. On the machine this was built for, the TV's saved layout was
  23.976 Hz in the wrong position. The fix is a one-off: activate that display,
  correct it in Settings, and the database is corrected for good. Note the
  failure mode — a *wrong layout restored*, never a failed switch.
- **Off is not the same as unplugged.** A display in standby keeps its
  hot-plug-detect line asserted, so it stays enumerable and switchable; this was
  confirmed with the TV physically powered off. A display unplugged at the cable
  or switched off at the wall may not, in which case applying a profile that
  names it fails with a message listing what *is* connected.
- **The output ceiling is real and reported as `ERROR_GEN_FAILURE`.** Asking for
  more simultaneous outputs than the card can drive fails with a code whose
  system text ("a device attached to the system is not functioning") is
  unhelpful, so the message spells out the likely cause.
- **Path priority is not preserved.** `SDC_ALLOW_PATH_ORDER_CHANGES` lets the
  database match on the *set* of outputs rather than their order, which is what
  makes the lookup reliable. If path priority matters to you, it isn't being
  controlled here.
- **One machine.** Verified on a single-GPU desktop with four displays, three
  active at a time. The `(adapter device path, target id)` design should
  generalise to multiple GPUs, but that is untested.

## Not in scope

Input-source switching — which video input a monitor itself is displaying — is
a different thing entirely, done over DDC/CI rather than by reconfiguring GPU
outputs. [ControlMyMonitor][cmm] does it well. The module layout here leaves
room for it, and monitor EDID ids are recorded with that in mind, but it isn't
implemented.

[cmm]: https://www.nirsoft.net/utils/control_my_monitor.html
