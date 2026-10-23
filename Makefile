# Peridot: build and install everything.
#   make && sudo make install          (PREFIX defaults to /usr/local)
#   make PREFIX=/usr && sudo make PREFIX=/usr install

PREFIX     ?= /usr/local
DESTDIR    ?=
BINDIR     ?= $(PREFIX)/bin
DATADIR    ?= $(PREFIX)/share
UDEVDIR    ?= /usr/lib/udev/rules.d
USERUNITDIR ?= /usr/lib/systemd/user
MODLOADDIR ?= /usr/lib/modules-load.d
AUTOSTARTDIR ?= /etc/xdg/autostart
LIBEXECDIR ?= $(PREFIX)/lib/peridot
BUILD      ?= build

.PHONY: all daemon app test install uninstall clean

all: daemon app

daemon:
	cd daemon && cargo build --release --locked

app:
	cmake -S app -B $(BUILD)/app -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=$(PREFIX)
	cmake --build $(BUILD)/app --parallel

test:
	cd daemon && cargo test --locked

$(BUILD)/peridotd.service: data/peridotd.service.in
	@mkdir -p $(BUILD)
	sed 's|@BINDIR@|$(BINDIR)|g' $< > $@

$(BUILD)/io.github.peridot.Peridot1.service: data/io.github.peridot.Peridot1.service.in
	@mkdir -p $(BUILD)
	sed 's|@BINDIR@|$(BINDIR)|g' $< > $@

$(BUILD)/70-peridot.rules: data/70-peridot.rules
	@mkdir -p $(BUILD)
	sed 's|@LIBEXECDIR@|$(LIBEXECDIR)|g' $< > $@

install: $(BUILD)/peridotd.service $(BUILD)/io.github.peridot.Peridot1.service $(BUILD)/70-peridot.rules
	install -Dm755 daemon/target/release/peridotd   $(DESTDIR)$(BINDIR)/peridotd
	install -Dm755 daemon/target/release/peridotctl $(DESTDIR)$(BINDIR)/peridotctl
	install -Dm755 $(BUILD)/app/peridot             $(DESTDIR)$(BINDIR)/peridot
	install -Dm644 $(BUILD)/70-peridot.rules        $(DESTDIR)$(UDEVDIR)/70-peridot.rules
	install -Dm755 data/usb-quirk                   $(DESTDIR)$(LIBEXECDIR)/usb-quirk
	install -Dm644 data/peridot-modules.conf        $(DESTDIR)$(MODLOADDIR)/peridot.conf
	install -Dm644 $(BUILD)/peridotd.service        $(DESTDIR)$(USERUNITDIR)/peridotd.service
	install -Dm644 $(BUILD)/io.github.peridot.Peridot1.service $(DESTDIR)$(DATADIR)/dbus-1/services/io.github.peridot.Peridot1.service
	install -Dm644 data/io.github.peridot.desktop   $(DESTDIR)$(DATADIR)/applications/io.github.peridot.desktop
	install -Dm644 data/io.github.peridot-tray.desktop $(DESTDIR)$(AUTOSTARTDIR)/io.github.peridot-tray.desktop
	install -Dm644 data/io.github.peridot.svg       $(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/io.github.peridot.svg

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/peridotd $(DESTDIR)$(BINDIR)/peridotctl $(DESTDIR)$(BINDIR)/peridot
	rm -f $(DESTDIR)$(UDEVDIR)/70-peridot.rules $(DESTDIR)$(MODLOADDIR)/peridot.conf
	rm -rf $(DESTDIR)$(LIBEXECDIR)
	rm -f $(DESTDIR)$(USERUNITDIR)/peridotd.service
	rm -f $(DESTDIR)$(DATADIR)/dbus-1/services/io.github.peridot.Peridot1.service
	rm -f $(DESTDIR)$(DATADIR)/applications/io.github.peridot.desktop $(DESTDIR)$(AUTOSTARTDIR)/io.github.peridot-tray.desktop
	rm -f $(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/io.github.peridot.svg

clean:
	rm -rf $(BUILD) daemon/target
