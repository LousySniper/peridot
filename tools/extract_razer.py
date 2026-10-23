#!/usr/bin/env python3
"""Extract a per-product capability table from the OpenRazer driver sources
(mice, keyboards and accessories such as mouse mats and docks).

Reads C source as text only. Output: JSON {pid: {...}} describing, for each
product, its class, which sysfs attributes the driver creates and, for each
command handler, which transaction id / builder variant / LED it uses.
"""
import json
import re
import sys

src_dir = sys.argv[1]
out_path = sys.argv[2]

# (class, driver file stem, probe function, function choosing the USB interface)
DRIVERS = [
    ("mouse", "razermouse_driver", "razer_mouse_probe", "razer_get_report"),
    ("keyboard", "razerkbd_driver", "razer_kbd_probe", "razer_get_report_params"),
    ("accessory", "razeraccessory_driver", "razer_accessory_probe", "razer_get_report"),
]
LED_RE = re.compile(r"razer_chroma_\w+\(\s*(?:VARSTORE|NOSTORE|razer_get_active_varstore\(device\))\s*,\s*(\w+_LED)")
lines = []


def load(stem):
    global lines
    lines = open(f"{src_dir}/driver/{stem}.c").read().split("\n")
    hdr = open(f"{src_dir}/driver/{stem}.h").read()
    return {m.group(1): int(m.group(2), 16) for m in re.finditer(r"#define\s+(USB_DEVICE_ID_RAZER_\w+)\s+0x([0-9A-Fa-f]+)", hdr)}


def func_bodies():
    """Yield (name, start_line, end_line) for each top-level function."""
    i = 0
    n = len(lines)
    while i < n:
        m = re.match(r"^(?:static\s+)?[\w\s\*]+?\b(\w+)\s*\(([^;]*)$", lines[i])
        if m and not lines[i].startswith(" ") and "(" in lines[i]:
            # find opening brace
            j = i
            while j < n and "{" not in lines[j] and ";" not in lines[j]:
                j += 1
            if j < n and "{" in lines[j] and ";" not in lines[i]:
                depth = 0
                k = j
                while k < n:
                    depth += lines[k].count("{") - lines[k].count("}")
                    if depth == 0:
                        break
                    k += 1
                yield m.group(1), j, k
                i = k + 1
                continue
        i += 1


def parse_switches(start, end):
    """Parse `switch (...usb_pid)` blocks; return list of {label: [statements]}."""
    results = []
    i = start
    while i <= end:
        if re.search(r"switch\s*\(\s*((dev|device)->usb_pid|(\w+->)*usb_dev->descriptor\.idProduct)\s*\)", lines[i]):
            depth = 0
            sw_depth = None
            active = []
            last_terminated = True
            seg = {}
            k = i
            while k <= end:
                ln = lines[k]
                opens = ln.count("{")
                closes = ln.count("}")
                if sw_depth is None:
                    depth += opens - closes
                    if opens:
                        sw_depth = depth
                    k += 1
                    continue
                stripped = ln.strip()
                if depth == sw_depth:
                    mcase = re.match(r"case\s+(USB_DEVICE_ID_RAZER_\w+)\s*:", stripped)
                    if mcase or stripped.startswith("default:"):
                        label = mcase.group(1) if mcase else "default"
                        if last_terminated:
                            active = [label]
                            last_terminated = False
                        else:
                            active.append(label)
                        seg.setdefault(label, [])
                        k += 1
                        continue
                    if stripped.startswith("break;") or re.match(r"return\b", stripped):
                        if stripped.startswith("return"):
                            for a in active:
                                seg[a].append(stripped)
                        last_terminated = True
                        active = []
                        k += 1
                        continue
                if stripped and active:
                    for a in active:
                        seg[a].append(stripped)
                depth += opens - closes
                if depth < sw_depth:
                    break
                k += 1
            results.append(seg)
            i = k
        i += 1
    return results


out = {}
for cls, stem, probe_fn, report_fn in DRIVERS:
    pid_of = load(stem)
    handlers = {}
    probe = None
    for name, s, e in func_bodies():
        if name == probe_fn:
            probe = (s, e)
        if name.startswith("razer_attr_") or name == report_fn:
            handlers[name] = parse_switches(s, e)

    devices = {}
    for sym, pid in pid_of.items():
        devices[sym] = {"pid": pid, "attrs": set(), "handlers": {}}

    # attributes created in probe, per product...
    in_switch = set()
    for seg in parse_switches(*probe):
        for label, stmts in seg.items():
            for st in stmts:
                m = re.search(r"dev_attr_(\w+)", st)
                if m:
                    in_switch.add(m.group(1))
                    if label in devices:
                        devices[label]["attrs"].add(m.group(1))
    # ...and for every product of the driver (outside the per-product switch)
    common = set(re.findall(r"CREATE_DEVICE_FILE\([^;]*dev_attr_(\w+)", "\n".join(lines[probe[0]:probe[1] + 1]))) - in_switch
    common -= {"version", "test", "device_type", "device_mode", "device_serial", "firmware_version", "kbd_layout"}
    for d in devices.values():
        if d["attrs"]:
            d["attrs"] |= common

    # per-handler details
    for hname, switches in handlers.items():
        for si, seg in enumerate(switches):
            for label, stmts in seg.items():
                if label not in devices and label != "default":
                    continue
                joined = " ".join(stmts)
                tids = re.findall(r"transaction_id\.id\s*=\s*(0x[0-9A-Fa-f]+)", joined)
                calls = sorted(set(re.findall(r"\b(razer_chroma_\w+|razer_get_usb_response|\w+_set_poll_rate|\w+_effect_\w+)\s*\(", joined)))
                waits = re.findall(r"(RAZER_\w+_WAIT_US)", joined)
                idx = re.findall(r"index\s*=\s*(0x[0-9A-Fa-f]+)", joined)
                idx += re.findall(r"razer_get_usb_response\(\s*hdev\s*,\s*(0x[0-9A-Fa-f]+)", joined)
                leds = sorted(set(LED_RE.findall(joined)))
                entry = {"tids": tids, "calls": calls, "waits": waits, "index": idx, "leds": leds}
                targets = [label] if label != "default" else ["default"]
                for t in targets:
                    if t == "default":
                        devices.setdefault("__default__", {"pid": -1, "attrs": set(), "handlers": {}})
                        devices["__default__"]["handlers"].setdefault(hname, []).append(entry)
                    else:
                        devices[t]["handlers"].setdefault(hname, []).append(entry)

    # names from read_device_type
    names = {}
    for seg in handlers.get("razer_attr_read_device_type", []):
        for label, stmts in seg.items():
            for st in stmts:
                m = re.search(r'device_type\s*=\s*"([^"\\]+)', st)
                if m:
                    names[label] = m.group(1)

    for sym, d in devices.items():
        if sym == "__default__":
            continue
        if not d["attrs"]:
            continue
        key = f"{d['pid']:04x}"
        if key in out:
            continue  # handled by an earlier driver
        out[key] = {
            "class": cls,
            "symbol": sym,
            "name": names.get(sym, sym),
            "attrs": sorted(d["attrs"]),
            "handlers": d["handlers"],
        }

json.dump(out, open(out_path, "w"), indent=1, sort_keys=True)
print(len(out), "devices")
