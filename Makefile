# Flea does not build plugins: `make` puts the compiled binaries where the plugin expects them.
CARGO ?= cargo
BIN := bin/flea-b2
MANAGER := bin/flea-b2-manager
SOURCES := Cargo.toml Cargo.lock $(wildcard src/*.rs) manager/Cargo.toml $(wildcard manager/src/*.rs)

.PHONY: all test keyring-test licenses install update clean

all: $(BIN) $(MANAGER)

# One build makes both: the plugin and the bucket manager's window.
$(BIN) $(MANAGER) &: $(SOURCES)
	$(CARGO) build --release --locked --workspace
	install -D -m 755 .target/release/flea-b2 $(BIN)
	install -D -m 755 .target/release/flea-b2-manager $(MANAGER)

# Unit tests, then the protocol tests (Python 3, development only) against a build with test hooks,
# then a check that THIRD-PARTY-LICENSES.txt still matches Cargo.lock.
test:
	$(CARGO) test --features test-hooks --locked
	$(CARGO) test -p flea-b2-manager --locked
	$(CARGO) build --features test-hooks --locked
	cd tests && PYTHONDONTWRITEBYTECODE=1 python3 -m unittest
	python3 tools/third-party-licenses --check

# The license and notice texts of every crate in the binaries; run after any change to Cargo.lock.
licenses:
	python3 tools/third-party-licenses

# The real keyring code against a throwaway gnome-keyring on a private session bus. The sandbox
# folders are set before dbus-run-session, so nothing in that session sees the user's keyring.
KEYRING_SANDBOX := $(CURDIR)/.target/keyring-sandbox

keyring-test:
	mkdir -p $(KEYRING_SANDBOX)/data $(KEYRING_SANDBOX)/run
	chmod 700 $(KEYRING_SANDBOX)/run
	env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
	  XDG_DATA_HOME=$(KEYRING_SANDBOX)/data XDG_RUNTIME_DIR=$(KEYRING_SANDBOX)/run FLEA_B2_KEYRING_SANDBOX=1 \
	  dbus-run-session -- sh -c 'printf throwaway | gnome-keyring-daemon --unlock --components=secrets >/dev/null \
	  && sleep 2 && $(CARGO) test --locked creds -- --ignored'

install: $(BIN) $(MANAGER)
	flea plugin validate "$(CURDIR)"
	flea plugin add "$(CURDIR)"
	flea plugin enable b2.bucket

# An installed folder plugin does not follow its source; removing it also drops its saved settings.
update: $(BIN) $(MANAGER)
	flea plugin remove b2.bucket
	flea plugin add "$(CURDIR)"
	flea plugin enable b2.bucket

clean:
	$(CARGO) clean
	rm -f $(BIN) $(MANAGER)
