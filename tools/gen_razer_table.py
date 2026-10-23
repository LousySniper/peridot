#!/usr/bin/env python3
"""Generate src/razer/devices.rs from razer.json (driver facts for mice,
keyboards and accessories) and the OpenRazer daemon's mouse.py (DPI limits,
polling rates). Reads files as data."""
import ast
import json
import sys
from collections import Counter

razer_json, mouse_py, out_rs = sys.argv[1], sys.argv[2], sys.argv[3]
devs = json.load(open(razer_json))

# ---- per-class metadata from the daemon (resolving inheritance) ----
tree = ast.parse(open(mouse_py).read())
classes = {}
for node in tree.body:
    if isinstance(node, ast.ClassDef):
        attrs = {}
        for st in node.body:
            if isinstance(st, ast.Assign) and len(st.targets) == 1 and isinstance(st.targets[0], ast.Name):
                try:
                    attrs[st.targets[0].id] = ast.literal_eval(st.value)
                except Exception:
                    pass
        bases = [b.id for b in node.bases if isinstance(b, ast.Name)]
        classes[node.name] = (bases, attrs)


def resolve(name, key, seen=None):
    seen = seen or set()
    if name in seen or name not in classes:
        return None
    seen.add(name)
    bases, attrs = classes[name]
    if key in attrs:
        return attrs[key]
    for b in bases:
        v = resolve(b, key, seen)
        if v is not None:
            return v
    return None


meta = {}
for cname in classes:
    pid = resolve(cname, "USB_PID")
    if isinstance(pid, int):
        meta[pid] = {
            "dpi_max": resolve(cname, "DPI_MAX"),
            "poll_rates": resolve(cname, "POLL_RATES"),
        }

WAITS = {
    "RAZER_MOUSE_WAIT_US": 600,
    "RAZER_NEW_MOUSE_RECEIVER_WAIT_US": 31000,
    "RAZER_ATHERIS_RECEIVER_WAIT_US": 400000,
    "RAZER_VIPER_MOUSE_RECEIVER_WAIT_US": 59900,
    "RAZER_BLACKWIDOW_CHROMA_WAIT_US": 600,
    "RAZER_BLACKWIDOW_V3_WIRELESS_WAIT_US": 4900,
    "RAZER_DEATHSTALKER_V2_WIRELESS_WAIT_US": 4900,
    "RAZER_FIREFLY_WAIT_US": 900,
    "RAZER_ACCESSORY_WAIT_US": 600,
    "RAZER_NEW_DEVICE_WAIT_US": 31000,
}
LED_IDS = {"ZERO_LED": 0x00, "SCROLL_WHEEL_LED": 0x01, "LOGO_LED": 0x04, "BACKLIGHT_LED": 0x05,
           "RIGHT_SIDE_LED": 0x10, "LEFT_SIDE_LED": 0x11}
# The interface a driver talks to when its per-product switch has no entry.
DEFAULT_IFACE = {"mouse": 0, "keyboard": 1, "accessory": 0}
CLASSES = {"mouse": "Mouse", "keyboard": "Keyboard", "accessory": "Accessory"}
ZONES = [  # prefix, led id, zone id, label
    ("logo_", 0x04, "logo", "Logo"),
    ("scroll_", 0x01, "scroll", "Scroll wheel"),
    ("left_", 0x11, "left", "Left side"),
    ("right_", 0x10, "right", "Right side"),
    ("backlight_", 0x05, "backlight", "Backlight"),
]
EFFECTS = [("none", 1), ("static", 2), ("breath", 4), ("spectrum", 8), ("wave", 16), ("reactive", 32)]


def h(dev, name):
    return dev["handlers"].get(name, [])


def first_tid(entries):
    for e in entries:
        for t in e["tids"]:
            return int(t, 16)
    return None


def calls(entries):
    out = []
    for e in entries:
        out += e["calls"]
    return out


rows = []
for pid_hex, d in sorted(devs.items()):
    pid = int(pid_hex, 16)
    attrs = set(d["attrs"])
    all_calls = " ".join(c for hs in d["handlers"].values() for e in hs for c in e["calls"])
    special = "orochi2011" in all_calls or "deathadder3_5g" in all_calls or "da3_5g" in all_calls
    flags = []
    if "charge_level" in attrs:
        flags.append("BATTERY")
    if "charge_status" in attrs:
        flags.append("CHARGING")
    if "device_idle_time" in attrs:
        flags.append("IDLE")
    if "charge_low_threshold" in attrs:
        flags.append("LOW_BATTERY")
    if "dpi" in attrs and not special:
        flags.append("DPI")
        if "razer_chroma_misc_set_dpi_xy_byte" in calls(h(d, "razer_attr_write_dpi")):
            flags.append("DPI_BYTE")
    if "dpi_stages" in attrs and not special:
        flags.append("DPI_STAGES")
    poll_handlers = h(d, "razer_attr_write_poll_rate")
    dual_tid = None
    if "poll_rate" in attrs and not special:
        flags.append("POLL")
        if "razer_chroma_misc_set_polling_rate2" in calls(poll_handlers):
            flags.append("POLL2")
            if len(poll_handlers) > 1:
                flags.append("POLL2_DUAL")
                dual_tid = first_tid(poll_handlers[1:])

    tids = []
    for name in [
        "razer_attr_read_charge_level",
        "razer_attr_write_dpi",
        "razer_attr_write_poll_rate",
        "razer_attr_read_dpi_stages",
        "razer_attr_read_device_idle_time",
        "razer_attr_read_firmware_version",
    ]:
        t = first_tid(h(d, name))
        if t is not None:
            tids.append(t)
    tid = Counter(tids).most_common(1)[0][0] if tids else 0xFF

    cls = d.get("class", "mouse")
    led = "None"
    led_entries = h(d, "razer_attr_write_matrix_effect_static_common")
    lc = calls(led_entries)
    if "razer_chroma_extended_matrix_effect_static" in lc:
        led = "Extended"
    elif "razer_chroma_mouse_extended_matrix_effect_static" in lc:
        led = "MouseExtended"
    elif "razer_chroma_standard_set_led_state" in lc or "razer_chroma_standard_set_led_effect" in lc:
        led = "Standard"
    led_tid = first_tid(led_entries)

    zones = []
    if led != "None":
        for prefix, led_id, zid, label in ZONES:
            mask = 0
            for eff, bit in EFFECTS:
                if f"{prefix}matrix_effect_{eff}" in attrs:
                    mask |= bit
            if mask:
                zones.append((led_id, zid, label, mask, f"{prefix}led_brightness" in attrs))
    if not zones and cls != "mouse":
        # Keyboards and accessories: one lighting zone for the whole device,
        # in whichever of the protocol generations its static handler uses.
        st = h(d, "razer_attr_write_matrix_effect_static")
        sc = calls(st)
        leds = [l for e in st for l in e.get("leds", [])]
        if "razer_chroma_extended_matrix_effect_static" in sc:
            led = "Extended"
        elif "razer_chroma_standard_matrix_effect_static" in sc:
            led = "StandardMatrix"
        elif "razer_chroma_standard_set_led_rgb" in sc:
            led = "Standard"
        led_id = LED_IDS.get(leds[0], 0x05) if leds else (0x05 if cls == "keyboard" else 0x00)
        mask = 0
        for eff, bit in EFFECTS:
            if f"matrix_effect_{eff}" in attrs:
                mask |= bit
        if led != "None" and mask:
            led_tid = first_tid(st) or led_tid
            zid, label = ("backlight", "Keyboard") if cls == "keyboard" else ("all", "Lighting")
            zones.append((led_id, zid, label, mask, "matrix_brightness" in attrs))
    if not zones and cls == "mouse":
        # Whole-device effects only (no per-zone files)
        whole = calls(h(d, "razer_attr_write_matrix_effect_static"))
        if "razer_chroma_extended_matrix_effect_static" in whole:
            mask = 0
            for eff, bit in EFFECTS:
                if f"matrix_effect_{eff}" in attrs:
                    mask |= bit
            if mask:
                led = "Extended"
                led_tid = first_tid(h(d, "razer_attr_write_matrix_effect_static")) or led_tid
                zones.append((0x00, "all", "Lighting", mask, "matrix_brightness" in attrs))
    if not zones:
        led = "None"

    gr = h(d, "razer_get_report") or h(d, "razer_get_report_params")
    iface = DEFAULT_IFACE[cls]
    wait = 600
    if gr:
        if gr[0]["index"]:
            iface = int(gr[0]["index"][0], 16)
        if gr[0]["waits"]:
            wait = WAITS.get(gr[0]["waits"][0], 600)
    m = meta.get(pid, {})
    dpi_max = m.get("dpi_max") or 16000
    polls = m.get("poll_rates")
    if not polls:
        polls = [125, 500, 1000]
    name = d["name"]
    wireless = "BATTERY" in flags
    if wireless:
        flags.append("WIRELESS")
    rows.append(
        dict(pid=pid, cls=CLASSES[cls], name=name, tid=tid, led_tid=led_tid or tid, iface=iface, wait=wait, dpi_max=dpi_max,
             flags=flags, led=led, zones=zones, polls=polls, dual_tid=dual_tid or tid)
    )

with open(out_rs, "w") as f:
    f.write("// @generated by tools/gen_razer_table.py from the OpenRazer driver sources.\n")
    f.write("// Facts about each product: which commands it accepts, its transaction id,\n")
    f.write("// USB interface, response delay, DPI ceiling and lighting zones.\n\n")
    f.write("use super::table::*;\n\n")
    f.write("pub static MODELS: &[Model] = &[\n")
    for r in rows:
        fl = " | ".join(r["flags"]) if r["flags"] else "0"
        zs = ", ".join(
            f'ZoneDef {{ led: 0x{z[0]:02x}, id: "{z[1]}", name: "{z[2]}", effects: {z[3]}, brightness: {"true" if z[4] else "false"} }}'
            for z in r["zones"]
        )
        polls = ", ".join(str(p) for p in r["polls"])
        f.write(
            f'    Model {{ pid: 0x{r["pid"]:04x}, class: Class::{r["cls"]}, name: "{r["name"]}", tid: 0x{r["tid"]:02x}, led_tid: 0x{r["led_tid"]:02x}, '
            f'poll_dual_tid: 0x{r["dual_tid"]:02x}, iface: {r["iface"]}, wait_us: {r["wait"]}, dpi_max: {r["dpi_max"]}, '
            f'flags: {fl}, led: LedProto::{r["led"]}, zones: &[{zs}], poll_rates: &[{polls}] }},\n'
        )
    f.write("];\n")
print(len(rows), "models written")
