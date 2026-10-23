# razer-cfg255 (third-party, GPL-2.0)

Written by Charles D'Aoust: <https://git.forked.io/charles/razer-blackshark-v3-quirk>
(the `module/` folder). It is copied here unchanged, by way of
wizardofth3moon/peripheral-studio, and keeps its own GPL-2.0 license (see the
SPDX line in `razer-cfg255.c`).

The BlackShark V3 family (V3, V3 Pro, V3 for Xbox) only enables its settings
and battery channel when the host's first configuration-descriptor read is 255
bytes long, as Windows does. Linux reads 9 bytes first, so on a stock kernel
the headset plays audio but ignores every command. This tiny kretprobe module
widens that one read for these devices only. It becomes unnecessary once the
usbcore quirk `USB_QUIRK_WINDOWS_CONFIG_REQ_SIZE` (usbcore.quirks flag `r`) is
in your kernel.

`install.sh` builds it with DKMS when a BlackShark V3 is plugged in (or was set
up before). After it loads, unplug the headset's dongle or cable and plug it
back in.
