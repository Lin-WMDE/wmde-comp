export prefix ?= /usr
sysconfdir ?= /etc
bindir = $(prefix)/bin
libdir = $(prefix)/lib
sharedir = $(prefix)/share

BINARY = wmde-comp
CARGO_TARGET_DIR ?= target
TARGET = debug
DEBUG ?= 0

.PHONY = all clean install uninstall vendor

ifeq ($(DEBUG),0)
	TARGET = release
	ARGS += --release
endif

VENDOR ?= 0
ifneq ($(VENDOR),0)
	ARGS += --offline --locked
endif

TARGET_BIN="$(DESTDIR)$(bindir)/$(BINARY)"

KEYBINDINGS_CONF="$(DESTDIR)$(sharedir)/wmde/fun.wmde.Settings.Shortcuts/v1/defaults"
TILING_EXCEPTIONS_CONF="$(DESTDIR)$(sharedir)/wmde/fun.wmde.Settings.WindowRules/v1/tiling_exception_defaults"

all: extract-vendor
	cargo build $(ARGS)

clean:
	cargo clean

distclean:
	rm -rf .cargo vendor vendor.tar target

vendor:
	mkdir -p .cargo
	cargo vendor | head -n -1 > .cargo/config
	echo 'directory = "vendor"' >> .cargo/config
	[ -n "$(SOURCE_GIT_HASH)" ] && printf '\n[env]\nGIT_HASH = "%s"\n' "$(SOURCE_GIT_HASH)" >> .cargo/config || true
	tar pcf vendor.tar vendor
	rm -rf vendor

extract-vendor:
ifeq ($(VENDOR),1)
	rm -rf vendor; tar pxf vendor.tar
endif

# wmde-comp ships ONLY the binary, its unit, and the default schemas.
# The wayland-session .desktop, wmde-session[-pre].target and the session
# helper script are owned exclusively by wmde-session (no install-bare-session
# target here) to avoid file conflicts between the two wmde-* packages.
install:
	install -Dm0755 "$(CARGO_TARGET_DIR)/$(TARGET)/$(BINARY)" "$(TARGET_BIN)"
	install -Dm0644 "data/keybindings.ron" "$(KEYBINDINGS_CONF)"
	install -Dm0644 "data/tiling-exceptions.ron" "$(TILING_EXCEPTIONS_CONF)"
	install -Dm0644 "data/wmde-comp.service" "$(DESTDIR)$(libdir)/systemd/user/wmde-comp.service"

uninstall:
	rm "$(TARGET_BIN)" "$(KEYBINDINGS_CONF)" "$(TILING_EXCEPTIONS_CONF)"
	rm "$(DESTDIR)$(libdir)/systemd/user/wmde-comp.service"
