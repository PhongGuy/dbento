# One command surface for contributors and CI: bare `make` installs Dbento, `make demo` opens
# it on a prefilled demo workspace, and `make check` is the full merge gate.
CARGO ?= cargo
# Build jobs for install and demo, low enough to leave room for other work on the machine.
JOBS ?= 4
# Install root for cargo, taken only from `make PREFIX=...`, never from the environment.
PREFIX :=

.DEFAULT_GOAL := install
.PHONY: install uninstall demo check fmt fmt-check lint test pilot-check pilot-timing timing-guard icons-check windows-check xwin-check xwin-build notices-check script-test package clean workflows-lint screenshots

install:
	CARGO="$(CARGO)" JOBS="$(JOBS)" DBENTO_PREFIX="$(PREFIX)" scripts/install.sh install

uninstall:
	CARGO="$(CARGO)" DBENTO_PREFIX="$(PREFIX)" scripts/install.sh uninstall

demo:
	CARGO="$(CARGO)" CARGO_BUILD_JOBS="$(JOBS)" scripts/demo.sh

check: fmt-check lint test

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

lint:
	$(CARGO) clippy --all-targets --all-features -- -Dwarnings

test:
	env -u WAYLAND_DISPLAY -u DISPLAY $(CARGO) test --workspace

# The pilot package sits outside the workspace because its host crate is private, so CI never builds it.
PILOT := --manifest-path tools/dbento-pilot/Cargo.toml
pilot-check:
	$(CARGO) fmt --check $(PILOT)
	$(CARGO) clippy $(PILOT) --all-targets --all-features -- -Dwarnings
	$(CARGO) build -p dbento-app --bin dbento-app --profile timing
	env -u WAYLAND_DISPLAY -u DISPLAY $(CARGO) test $(PILOT) -- --skip wheel_scroll_headless --skip wheel_scroll_session

# The two wheel recordings, which wait for a quiet machine and refuse a loaded one. A measured
# pass stamps the committed tree it ran on, which `timing-guard` asks for.
pilot-timing:
	$(CARGO) build -p dbento-app --bin dbento-app --profile timing
	tree=$$(scripts/timing-guard.sh tree) && \
	env -u WAYLAND_DISPLAY -u DISPLAY $(CARGO) test $(PILOT) --test wheel_scroll -- --exact wheel_scroll_headless wheel_scroll_session && \
	scripts/timing-guard.sh stamp "$$tree"

# Fails when a grid, scroll or render change since BASE (default origin/master) has no stamp.
timing-guard:
	scripts/timing-guard.sh

icons-check:
	$(CARGO) xtask icons --check

# CI must fail when the cross compiler is missing; a local run only skips and names it.
# A CI value that spells false counts as unset.
windows-check:
	$(CARGO) xtask check-windows $(if $(filter-out false False FALSE 0 no,$(strip $(CI))),--require)

# Local msvc cross-build from Linux through cargo-xwin, for the CLI, the app and xtask.
# Not in CI or `make check`; a Windows host builds the shipping binaries.
XWIN_TARGET := x86_64-pc-windows-msvc
XWIN_ARGS := --locked --target $(XWIN_TARGET) -p dbento-cli -p dbento-app -p xtask
# clang-cl 23 errors where cl.exe warns in pg_query 6.2.1's generic-msvc.h; the hyphenated
# name reaches only msvc C builds. Drop it once pg_query's vendored header builds under clang-cl.
# libduckdb-sys builds with DUCKDB_BUILD_LIBRARY, so DUCKDB_API is dllexport; the static-build
# define empties it for this static link. libduckdb v2.0.0 compiles without it, but no link without
# it has been tried, so keep it until one has.
XWIN_ENV := env 'CFLAGS_$(XWIN_TARGET)=-Wno-error=incompatible-pointer-types' \
	'CXXFLAGS_$(XWIN_TARGET)=-DDUCKDB_STATIC_BUILD'

xwin-check:
	$(XWIN_ENV) $(CARGO) xwin check $(XWIN_ARGS)

# Release-style build with the xwin-release profile (runtime shaders, see Cargo.toml); not for shipping.
xwin-build:
	$(XWIN_ENV) $(CARGO) xwin build --profile xwin-release $(XWIN_ARGS)

# cargo-about runs --offline, so fetch first: a fresh CI checkout has no crate sources yet.
notices-check:
	$(CARGO) fetch --locked
	$(CARGO) xtask notices --check

# Every bash suite under scripts/tests, each in its own bash so one cannot leak state into the next.
script-test:
	@set -e; for t in scripts/tests/*-test.sh; do echo "== $$t"; bash "$$t"; done

# Linux CLI tarball, .deb and AppImage, each verified, in target/dist/linux.
package:
	CARGO="$(CARGO)" CARGO_BUILD_JOBS="$(JOBS)" scripts/package-linux.sh

# The README screenshots in assets/screenshots, shot under gpui-pilot from the demo workspace.
screenshots:
	CARGO="$(CARGO)" scripts/screenshots.sh

clean:
	$(CARGO) clean

workflows-lint:
	scripts/actionlint.sh
