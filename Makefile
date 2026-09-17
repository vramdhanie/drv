# Local development install. `make install` builds the release binary,
# installs it to ~/.cargo/bin, and code-signs it with the self-signed
# "drv-signing" certificate so macOS Keychain approvals survive rebuilds
# (unsigned binaries get a new identity every build, which re-triggers
# the Keychain permission prompt).
#
# One-time setup for the signing identity: create a Self-Signed Root
# certificate of type "Code Signing" named drv-signing in Keychain
# Access, and set its Code Signing trust to Always Trust.

SIGN_IDENTITY ?= drv-signing
BIN := $(HOME)/.cargo/bin/drv

.PHONY: install build sign check

install:
	cargo install --path . --quiet
	@$(MAKE) --no-print-directory sign

sign:
	@if security find-identity -p codesigning -v | grep -q '"$(SIGN_IDENTITY)"'; then \
		codesign -f -s "$(SIGN_IDENTITY)" "$(BIN)" && \
		echo "signed $(BIN) as $(SIGN_IDENTITY)"; \
	else \
		echo "warning: signing identity '$(SIGN_IDENTITY)' not found — installed unsigned"; \
	fi

build:
	cargo build

check:
	cargo clippy
