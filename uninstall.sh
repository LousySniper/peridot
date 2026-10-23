#!/usr/bin/env bash
# Remove Peridot. Run as your normal user: ./uninstall.sh
# Your saved device settings in ~/.config/peridot are kept.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"
[ "$(id -u)" -ne 0 ] || { echo "Run as your normal user, without sudo." >&2; exit 1; }

pkill -x peridot 2>/dev/null || true
systemctl --user disable --now peridotd 2>/dev/null || true
sudo make PREFIX=/usr uninstall
sudo udevadm control --reload-rules
systemctl --user daemon-reload
echo "Peridot removed. Saved settings remain in ~/.config/peridot (delete that folder to remove them too)."
