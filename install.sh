#!/usr/bin/env bash
# Peridot installer for CachyOS / Arch. Run from the extracted folder as your
# normal user (not with sudo):  ./install.sh
# Safe to run again to upgrade.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"

say()  { printf '\n\033[1;32m==>\033[0m \033[1m%s\033[0m\n' "$*"; }
note() { printf '\033[1;33m  ! \033[0m%s\n' "$*"; }
die()  { printf '\n\033[1;31mError:\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -ne 0 ] || die "run ./install.sh as your normal user, without sudo (it asks for your password when needed)."
command -v pacman >/dev/null || die "this installer is for Arch-based systems (CachyOS, EndeavourOS, Manjaro...). See README.md for other systems."

say "Installing build dependencies"
pkgs=(base-devel cmake ninja qt6-base qt6-declarative qt6-svg)
if command -v rustup >/dev/null; then
    # rustup is installed: use it rather than the conflicting 'rust' package.
    cargo --version >/dev/null 2>&1 || rustup default stable
elif ! command -v cargo >/dev/null; then
    pkgs+=(rust)
fi
sudo pacman -S --needed --noconfirm "${pkgs[@]}"

say "Building (takes a minute or two)"
make PREFIX=/usr

say "Installing"
sudo make PREFIX=/usr install
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=hidraw --subsystem-match=input --subsystem-match=misc

# BlackShark V3 family: enable the kernel quirk they need, for headsets that are
# already plugged in (udev does it for later plug-ins).
v3_found=0
for d in /sys/bus/usb/devices/*; do
    [ "$(cat "$d/idVendor" 2>/dev/null)" = 1532 ] || continue
    case "$(cat "$d/idProduct" 2>/dev/null)" in
        0576|0577|0579|057a|0a4e|0a55)
            v3_found=1
            sudo /usr/lib/peridot/usb-quirk "$d" || true ;;
    esac
done
if [ "$v3_found" -eq 1 ]; then
    kver=$(uname -r | cut -d- -f1)
    if [ "$(printf '%s\n7.3\n' "$kver" | sort -V | head -n1)" != 7.3 ]; then
        note "Your BlackShark V3 needs Linux 7.3 or newer to accept settings (you're running $kver). Boot your newest kernel, or see third_party/razer-cfg255/README.md."
    fi
fi

reboot_needed=0
if [ ! -e /dev/uinput ] && ! sudo modprobe uinput 2>/dev/null; then
    if [ ! -d "/lib/modules/$(uname -r)" ]; then
        reboot_needed=1
    else
        note "Couldn't load the uinput module; shortcuts and media keys on mouse buttons won't work."
    fi
fi

say "Stopping tools that would fight over the same devices"
if systemctl --user is-active --quiet openrazer-daemon 2>/dev/null; then
    systemctl --user disable --now openrazer-daemon && echo "  stopped openrazer-daemon"
fi
if pgrep -x solaar >/dev/null; then
    pkill -x solaar && echo "  closed Solaar (remove it from autostart if it starts with your session)"
fi
if systemctl is-active --quiet ratbagd 2>/dev/null; then
    note "ratbagd (Piper) is running; stop it with: sudo systemctl disable --now ratbagd"
fi

say "Starting the Peridot service"
systemctl --user daemon-reload
systemctl --user enable peridotd >/dev/null 2>&1
systemctl --user restart peridotd
sleep 2
if ! systemctl --user is-active --quiet peridotd; then
    journalctl --user -u peridotd -n 20 --no-pager || true
    die "the service didn't start; the log above says why."
fi
peridotctl list || true

# An app left running from before (it lives in the tray) would keep the old
# version; restart it.
if pgrep -x peridot >/dev/null; then
    pkill -x peridot || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -x peridot >/dev/null || break; sleep 0.3; done
fi
if [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
    setsid -f peridot >/dev/null 2>&1 || true
fi

say "Done"
echo "  Peridot is in your app menu and starts in the tray at login."
echo "  If a device is missing, unplug and replug it (or its receiver) once."
if [ "$v3_found" -eq 1 ]; then
    note "BlackShark V3: if the app says the headset isn't answering, unplug its dongle (or cable) and plug it back in once."
fi
if [ "$reboot_needed" -eq 1 ]; then
    note "Reboot when convenient: your kernel was updated, and shortcuts/media keys on mouse buttons need the new one. Everything else works now."
fi
