# m4code task runner. `just` lists the recipes.

set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

default:
    @just --list

# ---- App (Rust + GPUI) ----------------------------------------------------------------------------

# Build the app.
build:
    cargo build --locked

# Run the app.
run:
    cargo run --locked

# Rust unit tests for the app.
test-app:
    cargo test --locked

# Render Home or Settings to target/screenshots/<scenario>.png without opening a desktop window.
screenshot $M4CODE_SCREENSHOT_SCENARIO="home":
    cargo test --locked --features screenshots --bin m4code screenshots::capture -- --exact --ignored --test-threads=1 --nocapture

# Check lints without changing files.
lint:
    cargo clippy --locked --all-targets -- -D warnings

# Test the Claude bridge's handling of background shells.
test-bridge:
    node bridge/background-shells.test.mjs

# ---- Mod (Claude Code plugin in mod/) -------------------------------------------------------------

# Tests for the mod: the *.test.ts files in mod/hooks. Needs the `claude` CLI on PATH.
test-mod:
    claude plugin test mod

# ---- Web (Astro site in web/, Bun) ----------------------------------------------------------------

# Install the web dependencies from bun.lock.
web-install:
    bun install --cwd web --frozen-lockfile

# Run the web dev server (installs dependencies first).
web: web-install
    bun --cwd web run dev

# Build the web site.
web-build: web-install
    bun --cwd web run build

# Serve the built web site.
web-preview:
    bun --cwd web run preview

# ---- Everything -----------------------------------------------------------------------------------

# Every test: the app and the mod.
test: test-app test-bridge test-mod

# What CI checks, plus the mod and the web build.
ci: build test-app test-bridge web-build test-mod
