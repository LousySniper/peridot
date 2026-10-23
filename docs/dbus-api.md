# Peridot D-Bus API

Service `io.github.peridot.Peridot1` on the **session** bus, object
`/io/github/peridot/Peridot1`, interface `io.github.peridot.Manager1`.

Every payload is a JSON string, so any language can use the API without
generated bindings.

## Methods

| Method | In | Out | Notes |
|---|---|---|---|
| `GetVersion` | | `s` version | |
| `GetStatus` | | `s` JSON | `{version, devices, uinput, uinput_error, config_dir, warnings[]}` |
| `ListDevices` | | `s` JSON array | device snapshots, in connection order |
| `GetDevice` | `s` id | `s` JSON | |
| `SetSetting` | `s` id, `s` key, `s` JSON value | `s` updated device | see keys below |
| `Refresh` | `s` id | `s` updated device | re-reads battery |
| `ForgetDevice` | `s` id | | deletes saved settings |
| `GetActions` | | `s` JSON array | `{id, label, group}` button actions for menus |

Errors use these names (message is human-readable):
`io.github.peridot.Error.NotFound`, `.InvalidArgs`, `.NotSupported`,
`.Device`, `.Offline`. **Offline** means the device is asleep; the setting
was saved and is applied when it wakes up.

## Signals

`DeviceAdded(s device)`, `DeviceChanged(s device)`, `DeviceRemoved(s id)`.

## Device snapshot

```json
{
  "id": "logitech-4a1c0b77",          // stable; settings are saved under it
  "name": "PRO X Wireless",
  "vendor": "logitech",                // or "razer"
  "kind": "mouse",                     // mouse | headset | keyboard | controller | other
  "connection": "receiver",            // usb | receiver | bluetooth
  "online": true,                      // false = paired but asleep
  "model": "046d:4093", "via": "Lightspeed Receiver",
  "serial": "4A1C0B77", "firmware": "RQM 12.03.B002A", "protocol": "HID++ 4.5",
  "battery": {"percent": 85, "level": "full", "charging": false, "full": false,
              "charge_error": false, "voltage": null, "estimated": false},
  "dpi": {"value": 800, "min": 100, "max": 25600, "step": 50, "list": [],
          "stages": [400, 800, 1600, 3200], "active_stage": 1, "max_stages": 5,
          "hardware_stages": false},
  "polling_rate": {"value": 1000, "options": [125, 250, 500, 1000]},
  "lighting": {"zones": [{"id": "logo", "name": "Logo",
                "effects": ["off", "static", "breathing", "spectrum"],
                "current": {"effect": "static", "color": "#9bd13e", "color2": null,
                            "speed": 50, "direction": "right"},
                "brightness": null, "dual_color": false}]},
  "buttons": {"controls": [{"id": "0053", "name": "Back", "action": "default", "remappable": true}]},
  "audio": {"sidetone": {"value": 35, "min": 0, "max": 100, "step": 1},
            "equalizer": {"bands": [{"freq": 100, "gain": 0}], "min": -12, "max": 12, "software": false},
            "auto_off": {"value": 30, "options": [0, 5, 10, 15, 20, 30, 45, 60, 90]},
            "modes": [{"id": "anc", "name": "Noise cancelling", "hint": "...", "value": "off",
                       "options": [{"value": "off", "label": "Off"}, {"value": "anc", "label": "On"}]}]},
  "power": {"idle_time": {"value": 300, "min": 60, "max": 900, "step": 60},
            "low_battery": {"value": 15, "min": 5, "max": 25, "step": 1}},
  "onboard_mode": "host",              // Logitech gaming mice: "host" | "onboard"
  "notes": ["..."]
}
```

Sections a device doesn't support are `null`.

`audio.equalizer.presets` lists presets for the headset as
`{"id", "name", "gains"}` (empty when the app should offer generic ones).

`sticks` (controllers) is `{"enabled", "left": {"deadzone", "center"}, "right": ...,
"active", "error"}`; `active` means games are getting the corrected copy.

`audio.equalizer.custom` is the curve the user last tuned by hand, if any.

`audio.pc` is present for USB headsets and holds what Peridot does on the
computer: `{"surround": false, "active": false, "format": "48 kHz · 16-bit ·
stereo", "error": null}`. `active` means the headset's PipeWire filter is
running.

`audio.equalizer.software` is true when the headset's firmware won't take EQ
changes and Peridot applies the EQ through a PipeWire filter instead (a
WirePlumber smart filter on the headset's sink, node `peridot_eq_<id>`).

## Setting keys

| key | value |
|---|---|
| `dpi` | number |
| `dpi_stages` | `{"stages": [400, 800, 1600], "active": 1}` (active is 0-based) |
| `dpi_cycle` | `"up"` / `"down"` |
| `dpi_shift` | `true` while held, `false` on release |
| `polling_rate` | Hz, one of `polling_rate.options` |
| `lighting` | `{"zone": "logo" \| "all", "effect": "...", "color": "#rrggbb", "color2": "#rrggbb", "speed": 0-100, "direction": "left" \| "right"}` |
| `brightness` | `{"zone": "logo" \| "all", "value": 0-100}` (Logitech: applies to all zones) |
| `button` | `{"control": "0053", "action": "key:ctrl+c"}` |
| `sidetone` | 0-100 |
| `equalizer` | array of gains in dB, one per band; or `{"gains": [...], "custom": true}` for a curve the user tuned, which is also kept as `audio.equalizer.custom` |
| `auto_off` | minutes, 0 = never |
| `surround` | `true` / `false`: virtual 7.1 surround on the computer |
| `sticks` | controllers: `{"enabled": true, "left": {"deadzone": 8}, "right": {"deadzone": 8}}`, any part of it (dead zones 0-30 %, `center` from calibration) |
| `calibrate_sticks` | controllers: `true`; measures the resting sticks for 2 s, sets centres and dead zones and turns the fix on |
| `mode` | `{"id": "anc", "value": "ambient"}`, one of `audio.modes` (not saved: the headset's own buttons change these) |
| `idle_time` | seconds (60-900) |
| `low_battery` | percent (5-25) |
| `onboard_mode` | `"host"` or `"onboard"` |

### Button actions

`default`, `disabled`, `button:left|right|middle|back|forward`,
`media:play_pause|next|previous|stop|volume_up|volume_down|mute|mic_mute|screenshot|...`,
`dpi:up|down|shift`, and any key combination as `key:<mods>+<key>`
(modifiers: ctrl, shift, alt, super; keys: a-z, 0-9, f1-f24, arrows, home, end,
pageup, pagedown, insert, delete, space, enter, esc, tab, ...).

## Examples

```sh
busctl --user call io.github.peridot.Peridot1 /io/github/peridot/Peridot1 \
  io.github.peridot.Manager1 SetSetting sss logitech-4a1c0b77 dpi 1600

gdbus monitor --session --dest io.github.peridot.Peridot1
```
