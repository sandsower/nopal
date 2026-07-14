SHELL := /bin/sh

.DEFAULT_GOAL := app

HOST_OS := $(shell uname -s)
APP_VERSION := $(shell sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml)
CARGO_TARGET_DIR := $(CURDIR)/target
RELEASE_DIR := $(CARGO_TARGET_DIR)/release

ifeq ($(HOST_OS),Darwin)
HOST_APP_TARGET := macos
else ifeq ($(HOST_OS),Linux)
HOST_APP_TARGET := linux
else
HOST_APP_TARGET := unsupported-host
endif

.PHONY: app macos linux unsupported-host clean-app help

app: $(HOST_APP_TARGET)

macos:
	@if [ "$(HOST_OS)" != "Darwin" ]; then \
		echo "macos target requires macOS; run 'make' to build for $(HOST_OS)" >&2; \
		exit 2; \
	fi
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo build --locked --release \
		-p nopal-cli -p nopal-desktop-spike
	@set -eu; \
	stage="Nopal.app.tmp"; \
	rm -rf "$$stage"; \
	trap 'rm -rf "$$stage"' EXIT HUP INT TERM; \
	install -d "$$stage/Contents/MacOS"; \
	install -m 0755 "$(RELEASE_DIR)/nopal-field-native" "$$stage/Contents/MacOS/nopal-field-native"; \
	install -m 0755 "$(RELEASE_DIR)/nopal" "$$stage/Contents/MacOS/nopal"; \
	printf '%s\n' \
		'<?xml version="1.0" encoding="UTF-8"?>' \
		'<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
		'<plist version="1.0">' \
		'<dict>' \
		'  <key>CFBundleDisplayName</key>' \
		'  <string>Nopal</string>' \
		'  <key>CFBundleExecutable</key>' \
		'  <string>nopal-field-native</string>' \
		'  <key>CFBundleIdentifier</key>' \
		'  <string>com.sandsower.nopal</string>' \
		'  <key>CFBundleInfoDictionaryVersion</key>' \
		'  <string>6.0</string>' \
		'  <key>CFBundleName</key>' \
		'  <string>Nopal</string>' \
		'  <key>CFBundlePackageType</key>' \
		'  <string>APPL</string>' \
		'  <key>CFBundleShortVersionString</key>' \
		'  <string>$(APP_VERSION)</string>' \
		'  <key>CFBundleVersion</key>' \
		'  <string>$(APP_VERSION)</string>' \
		'  <key>NSHighResolutionCapable</key>' \
		'  <true/>' \
		'</dict>' \
		'</plist>' > "$$stage/Contents/Info.plist"; \
	plutil -lint "$$stage/Contents/Info.plist" >/dev/null; \
	rm -rf Nopal.app; \
	mv "$$stage" Nopal.app; \
	trap - EXIT HUP INT TERM; \
	printf 'Built %s/Nopal.app\n' "$(CURDIR)"

linux:
	@if [ "$(HOST_OS)" != "Linux" ]; then \
		echo "linux target requires Linux; run 'make' to build for $(HOST_OS)" >&2; \
		exit 2; \
	fi
	CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo build --locked --release \
		-p nopal-cli -p nopal-desktop-spike
	@set -eu; \
	stage="Nopal-linux.tmp"; \
	rm -rf "$$stage"; \
	trap 'rm -rf "$$stage"' EXIT HUP INT TERM; \
	install -d "$$stage"; \
	install -m 0755 "$(RELEASE_DIR)/nopal-field-native" "$$stage/nopal-field-native"; \
	install -m 0755 "$(RELEASE_DIR)/nopal" "$$stage/nopal"; \
	rm -rf Nopal-linux; \
	mv "$$stage" Nopal-linux; \
	trap - EXIT HUP INT TERM; \
	printf 'Built %s/Nopal-linux/nopal-field-native\n' "$(CURDIR)"

unsupported-host:
	@echo "unsupported host OS: $(HOST_OS)" >&2
	@exit 2

clean-app:
	rm -rf Nopal.app Nopal.app.tmp Nopal-linux Nopal-linux.tmp

help:
	@printf '%s\n' \
		'make           Build the desktop app for this host OS' \
		'make macos     Build ./Nopal.app on macOS' \
		'make linux     Build ./Nopal-linux/nopal-field-native on Linux' \
		'make clean-app Remove generated desktop app bundles'
