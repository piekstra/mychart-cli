BIN := mychart
CARGO ?= cargo

.PHONY: build release test lint fmt fmt-check clean install deps smoke verify dev mock

# Every target that produces a binary signs it. `cargo build` and
# `cargo install` ad-hoc sign, minting a new code identity each time, and
# macOS scopes keychain "Always Allow" grants to that identity: an unsigned
# rebuild silently revokes the grant and the next run prompts again.
build: SIGN_TARGET = target/debug/$(BIN)
build:
	$(CARGO) build
	@$(SIGN)

release: SIGN_TARGET = target/release/$(BIN)
release:
	$(CARGO) build --release
	@$(SIGN)

test:
	$(CARGO) test

lint:
	$(CARGO) clippy --all-targets -- -D warnings

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

clean:
	$(CARGO) clean

deps:
	$(CARGO) fetch

# `--locked` builds from Cargo.lock, the versions the tests and audit ran on.
install: SIGN_TARGET = $${CARGO_INSTALL_ROOT:-$$HOME/.cargo}/bin/$(BIN)
install:
	$(CARGO) install --path . --locked --force
	@$(SIGN)

dev: SIGN_TARGET = target/debug/$(BIN)
dev:
	$(CARGO) build
	@$(SIGN)

# A local stand-in for an Epic organization (directory, SMART, FHIR) serving
# one synthetic patient, for dogfooding. CONTRIBUTING.md shows how to point
# mychart at it.
mock:
	$(CARGO) run --example mock_epic

# Offline smoke test on the release binary. It never reads the real keychain:
# MYCHART_KEYCHAIN_SERVICE names a service that holds nothing, and the config
# and cache live under target/smoke.
SMOKE_DIR := $(CURDIR)/target/smoke
SMOKE := MYCHART_KEYCHAIN_SERVICE=piekstra.mychart.hermetic-tests \
	XDG_CONFIG_HOME=$(SMOKE_DIR)/config XDG_CACHE_HOME=$(SMOKE_DIR)/cache \
	./target/release/$(BIN)
smoke: release
	@$(SMOKE) --version >/dev/null
	@$(SMOKE) info --json >/dev/null
	@$(SMOKE) completions bash >/dev/null
	@$(SMOKE) auth status --json >/dev/null
	@$(SMOKE) patients list --json >/dev/null
	@for c in "orgs list" "orgs search" "orgs refresh" "auth login" "auth status" \
		"auth logout" "patients list" "records export" "documents list" \
		"documents download" "api" "config set"; do \
		$(SMOKE) $$c --help >/dev/null || exit 1; done
	@echo "smoke ok"

verify: fmt-check lint test smoke

define SIGN
if [ -x "$$HOME/Dev/cli-common/scripts/dev-sign.sh" ]; then \
	"$$HOME/Dev/cli-common/scripts/dev-sign.sh" "$(SIGN_TARGET)"; \
else echo "cli-common/scripts/dev-sign.sh not found — $(SIGN_TARGET) left ad-hoc signed"; fi
endef
