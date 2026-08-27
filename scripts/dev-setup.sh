#!/usr/bin/env bash
# Stratum contributor setup — macOS and Linux (design 08; W22).
# Idempotent: safe to re-run. Installs nothing system-wide without saying so.
set -euo pipefail

say() { printf '\n== %s\n' "$*"; }
need() { command -v "$1" >/dev/null 2>&1; }

CROSS=0
for arg in "$@"; do
  case "$arg" in
    --cross) CROSS=1 ;;
    *) echo "usage: $0 [--cross]" >&2; exit 2 ;;
  esac
done

say "Rust toolchain (pinned by rust-toolchain.toml)"
if ! need rustup; then
  echo "rustup is required: https://rustup.rs" >&2
  exit 1
fi
# Respects rust-toolchain.toml (1.96.0 + rustfmt + clippy). The toolchain file
# deliberately pins no targets — they are added explicitly, here and in CI, so
# a host-only machine never pulls ~1 GB of std it will not use.
rustup show active-toolchain || rustup toolchain install

say "wasm target (W11a's stratum-wasm)"
rustup target add wasm32-unknown-unknown

say "Node + pnpm (frontend, apps/desktop)"
if ! need node; then
  echo "Node >= 22.12 is required (nvm install 22)" >&2
  exit 1
fi
if ! need pnpm; then
  corepack enable && corepack prepare pnpm@9.15.0 --activate
fi

say "Frontend dependencies"
(cd "$(dirname "$0")/../apps/desktop" && pnpm install --frozen-lockfile)

case "$(uname -s)" in
  Linux)
    say "Linux system libraries (Tauri v2 / WebKitGTK 4.1)"
    if need apt-get; then
      sudo apt-get update
      sudo apt-get install -y \
        libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf \
        build-essential curl wget file libssl-dev
    else
      echo "Non-apt distro: install webkit2gtk4.1, gtk3, librsvg2 and patchelf" >&2
    fi
    ;;
  Darwin)
    say "Xcode command-line tools"
    xcode-select -p >/dev/null 2>&1 || xcode-select --install
    ;;
esac

say "cargo-nextest (test runner CI uses)"
need cargo-nextest || cargo install cargo-nextest --locked

if [ "$CROSS" -eq 1 ]; then
  say "Cross-target checks (opt-in: pulls std for two extra triples)"
  # `cargo check --target <triple>` runs every build script in the graph, and
  # some of them compile C (blake3 workspace-wide; aws-lc-sys via stratum-ai →
  # reqwest → rustls; mimalloc in the desktop host). See packaging/README.md,
  # "Cross-target checks", and `cargo xtask dist cross-check`, which drives both
  # triples with whatever each needs from this host.
  rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc
  if [ "$(uname -s)" = "Darwin" ]; then
    say "zig as the x86_64-linux-gnu cross C compiler (and the msvc archiver)"
    if ! need brew; then
      echo "Homebrew is required for --cross on macOS: https://brew.sh" >&2
      exit 1
    fi
    need zig || brew install zig
    bindir="$(brew --prefix)/bin"
    crossdir="$(cd "$(dirname "$0")/../packaging/cross" && pwd)"
    echo "Installing compiler shims system-wide into $bindir (saying so):"
    for tool in gcc g++ ar ranlib; do
      # Both spellings cc-rs probes for this target.
      for prefix in x86_64-linux-gnu x86_64-unknown-linux-gnu; do
        install -m 0755 "$crossdir/x86_64-linux-gnu-$tool" "$bindir/$prefix-$tool"
        echo "  $bindir/$prefix-$tool"
      done
    done
    echo
    echo "cargo check --target x86_64-unknown-linux-gnu now works from this Mac, in full."
    echo "x86_64-pc-windows-msvc: run \`cargo xtask dist cross-check\` — it sets the two"
    echo "env vars blake3 needs (clang + zig's lib driver) and type-checks every default"
    echo "member except those whose C #includes the Windows CRT/SDK headers (stratum-ai"
    echo "via aws-lc-sys), which only a Windows host has; it reports PARTIAL and names"
    echo "them. Fetching those headers off-Windows (cargo-xwin) means accepting"
    echo "Microsoft's license — a per-user decision, not a setup script's. CI builds"
    echo "that target natively on windows-2022."
  fi
fi

say "Done. Useful entry points:"
cat <<'DONE'
  ./scripts/dev-setup.sh --cross  # opt-in cross-target check toolchain
  cargo xtask dist cross-check    # type-check the Linux and Windows triples
  cargo xtask layering            # the invariant checks CI runs
  cargo nextest run --workspace   # tests
  cd apps/desktop && pnpm dev     # frontend dev server
  cargo run -p stratum-desktop    # the desktop host (dev, needs pnpm dev)
  cargo xtask dist stage && (cd apps/desktop && \
    APPLE_SIGNING_IDENTITY="-" pnpm dlx @tauri-apps/cli build)
                                  # a local release bundle, ad-hoc signed on
                                  # macOS (see packaging/README.md)
DONE
