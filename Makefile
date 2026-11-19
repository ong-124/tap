BUILD_MISC_DIR = misc
TARGET = tap
TARGET_DIR = target/release
TARGET_DIR_DEBIAN = target/debian
TARGET_DIR_OSX = $(TARGET_DIR)/osx
RELEASE_DIR = release

APP_NAME = Tap.app
APP_TEMPLATE = $(BUILD_MISC_DIR)/osx/$(APP_NAME)
APP_BINARY = $(TARGET_DIR)/$(TARGET)
APP_BINARY_DIR = $(TARGET_DIR_OSX)/$(APP_NAME)/Contents/MacOS
APP_EXTRAS_DIR = $(TARGET_DIR_OSX)/$(APP_NAME)/Contents/Resources
TERMINFO = $(BUILD_MISC_DIR)/tap.terminfo

all: install run

run:
	cargo run -p tapterm --release

# OXS: optionally you can run "/bin/launchctl setenv MTL_HUD_ENABLED 1"
dev:
	MTL_HUD_ENABLED=1 cargo run -p tapterm --features wgpu

dev-debug:
	MTL_HUD_ENABLED=1 TAP_LOG_LEVEL=debug make dev

dev-debug-wayland:
	TAP_LOG_LEVEL=debug cargo run -p tapterm --no-default-features --features=wayland

dev-debug-x11:
	TAP_LOG_LEVEL=debug cargo run -p tapterm --no-default-features --features=x11

run-wasm:
	cargo build -p tapterm --target wasm32-unknown-unknown --lib
	cargo run -p tapterm-wasm

dev-watch:
	#cargo install cargo-watch
	cargo watch -- cargo run -p tapterm

install:
	cargo fetch

build: install
	RUSTFLAGS='-C link-arg=-s' cargo build --release

# install:
# rustup target add x86_64-apple-darwin
# rustup target add aarch64-apple-darwin
$(TARGET)-universal:
	# Note: Catalina is 10.15 and Big Sur is 11.0
	RUSTFLAGS='-C link-arg=-s' MACOSX_DEPLOYMENT_TARGET="10.15" cargo build --release --target=x86_64-apple-darwin
	RUSTFLAGS='-C link-arg=-s' MACOSX_DEPLOYMENT_TARGET="11.0" cargo build --release --target=aarch64-apple-darwin
	@lipo target/{x86_64,aarch64}-apple-darwin/release/$(TARGET) -create -output $(APP_BINARY)

app-universal: $(APP_NAME)-universal ## Create a universal Tap.app
$(APP_NAME)-%: $(TARGET)-%
	@mkdir -p $(APP_BINARY_DIR)
	@mkdir -p $(APP_EXTRAS_DIR)
	@cp -fRp $(APP_TEMPLATE) $(TARGET_DIR_OSX)
	@cp -fp $(APP_BINARY) $(APP_BINARY_DIR)
	@touch -r "$(APP_BINARY)" "$(TARGET_DIR_OSX)/$(APP_NAME)"

install-terminfo:
	@tic -xe xterm-tap,tap -o $(APP_EXTRAS_DIR) $(TERMINFO)

.PHONY: libtap-ctest libtap-xcframework
libtap-ctest:
	cargo build -p libtap
	@mkdir -p target/libtap
	cc libtap/ctest/main.c target/debug/liblibtap.a \
		-o target/libtap/ctest \
		-framework CoreFoundation -framework Foundation -framework AppKit \
		-framework CoreGraphics -framework CoreText -framework Metal \
		-framework QuartzCore -framework CoreVideo -liconv -lc++
	target/libtap/ctest

LIBTAP_XCF = target/libtap/TapKit.xcframework
libtap-xcframework:
	cargo build -p libtap --profile libtap --target aarch64-apple-darwin
	@rm -rf $(LIBTAP_XCF)
	@mkdir -p $(LIBTAP_XCF)/macos-arm64/Headers
	@cp target/aarch64-apple-darwin/libtap/liblibtap.a \
		$(LIBTAP_XCF)/macos-arm64/libtap.a
	@cp libtap/include/libtap.h libtap/include/module.modulemap \
		$(LIBTAP_XCF)/macos-arm64/Headers/
	@cp libtap/xcframework-info.plist $(LIBTAP_XCF)/Info.plist
	@echo "Created '$(LIBTAP_XCF)'"

release-macos: app-universal
	@codesign --remove-signature "$(TARGET_DIR_OSX)/$(APP_NAME)"
	@codesign --force --deep --sign - "$(TARGET_DIR_OSX)/$(APP_NAME)"
	@echo "Created '$(APP_NAME)' in '$(TARGET_DIR_OSX)'"
	mkdir -p $(RELEASE_DIR)
	cp -rf ./target/release/osx/* ./release/
	cd ./release && zip -r ./macos-unsigned.zip ./*

release-macos-signed:
	$(eval VERSION = $(shell echo $(version)))
	$(if $(strip $(VERSION)),make release-macos-signed-app, make version-not-found)

release-macos-signed-app:
	@make install-terminfo
	@make app-universal
	@echo "Releasing Tap v$(version)"
	@codesign --force --deep --options runtime --sign "Developer ID Application: Hugo Amorim" "$(TARGET_DIR_OSX)/$(APP_NAME)"
	mkdir -p $(RELEASE_DIR) && cp -rf ./target/release/osx/* ./release/
	@ditto -c -k --keepParent ./release/$(APP_NAME) ./release/Tap-v$(version).zip
	@xcrun notarytool submit ./release/Tap-v$(version).zip --keychain-profile "Hugo Amorim" --wait
	rm -rf ./release/$(APP_NAME)
	@unzip ./release/Tap-v$(version).zip -d ./release
	@echo "Please verify if 'Tap.App/Contents/Resources/72/tap' exists before create-dmg"

install-macos: release-macos
	rm -rf /Applications/$(APP_NAME)
	mv ./release/$(APP_NAME) /Applications/

version-not-found:
	@echo "Tap version was not specified"
	@echo " - usage: $ make release-macos-signed version=0.0.0"

# e.g: make update-version old-version=0.1.13 new-version=0.1.12
update-version:
	@echo "Switching from $(old-version) to $(new-version)"
	find Cargo.toml -type f -exec sed -i '' 's/$(old-version)/$(new-version)/g' {} \;
	find CHANGELOG.md -type f -exec sed -i '' 's/Unreleased/Unreleased\n\n- TBD\n\n## $(new-version)/g' {} \;
	find $(BUILD_MISC_DIR)/windows/tap.wxs -type f -exec sed -i '' 's/$(old-version)/$(new-version)/g' {} \;
	find $(APP_TEMPLATE)/Contents/Info.plist -type f -exec sed -i '' 's/$(old-version)/$(new-version)/g' {} \;

release-macos-dmg:
# 	Using https://www.npmjs.com/package/create-dmg
	cd ./release && create-dmg $(APP_NAME) --dmg-title="Tap ${version}" --overwrite

bump-brew:
	brew bump-cask-pr tap --version ${version}

# TODO: Move to bin path
release-x11:
	RUSTFLAGS='-C link-arg=-s' cargo build --release --no-default-features --features=x11
	target/release/tap
release-wayland:
	RUSTFLAGS='-C link-arg=-s' cargo build --release --no-default-features --features=wayland
	target/release/tap

# Debian
# cargo install cargo-deb
# To install: sudo release/debian/tap_<version>_<architecture>_<feature>.deb
# e.g: sudo release/debian/tap_0.0.13_arm64_wayland.deb
release-debian-x11:
	cargo deb -p tapterm -- --no-default-features --features=x11
	mkdir -p $(RELEASE_DIR)/debian/x11
	mv $(TARGET_DIR_DEBIAN)/* $(RELEASE_DIR)/debian/x11/
	cd $(RELEASE_DIR)/debian/x11 && rename 's/.deb/_x11.deb/g' *

release-debian-wayland:
	cargo deb -p tapterm -- --no-default-features --features=wayland
	mkdir -p $(RELEASE_DIR)/debian/wayland
	mv $(TARGET_DIR_DEBIAN)/* $(RELEASE_DIR)/debian/wayland/
	cd $(RELEASE_DIR)/debian/wayland && rename 's/.deb/_wayland.deb/g' *

# Release and Install
install-debian-x11:
	cargo install cargo-deb
	cargo deb -p tapterm --install -- --release --no-default-features --features=x11
install-debian-wayland:
	cargo install cargo-deb
	cargo deb -p tapterm --install -- --release --no-default-features --features=wayland

# cargo install cargo-wix
# https://github.com/volks73/cargo-wix
release-windows:
	cargo wix -p tapterm

lint:
	cargo fmt -- --check --color always
	cargo clippy --all-targets --all-features -- -D warnings

test:
	make lint
	RUST_BACKTRACE=full cargo test --release

publish-crates: build
	# Note: cargo publish is only supported from >=1.90
	cargo publish --workspace
