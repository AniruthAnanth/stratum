# packaging/ — W22

Everything the bundler and the release pipeline reference from outside
`apps/desktop/src-tauri`. Owned by W22 (docs/ownership.toml).

| Path | Consumed by |
|---|---|
| `icons/` | `bundle.icon` in every `tauri.*.conf.json`; `icon.icns` (macOS), `icon.ico` (Windows), PNGs (Linux). Source of truth is `app-icon-source.svg` (the landing page's mark: three teal strata bars on the dark ground). Regenerate with `rsvg-convert -w 1024 -h 1024 packaging/icons/app-icon-source.svg -o packaging/icons/app-icon-source.png && pnpm dlx @tauri-apps/cli icon packaging/icons/app-icon-source.png -o packaging/icons`, then delete the generated `android/` and `ios/` sets. |
| `macos/Info.additions.plist` | Copied to `apps/desktop/src-tauri/Info.plist` (untracked) by `cargo xtask dist stage`; the Tauri bundler merges it into the generated Info.plist. Carries the `LSHandlerRank = Alternate` file associations, imported Stata UTIs and the `stratum://` URL scheme. |
| `windows/file-assoc.nsh` | `bundle.windows.nsis.installerHooks` — never-steal file associations + Default Programs + uninstall cleanup. |
| `windows/wix/file-assoc.wxs` | `bundle.windows.wix.fragmentPaths` — the same registration for the MSI. |
| `linux/*.desktop`, `linux/*.xml` | Mapped into `.deb`/`.rpm` via `bundle.linux.{deb,rpm}.files`; MIME + desktop + AppStream metainfo. |
| `linux/deb-postinst.sh`, `linux/deb-postrm.sh` | deb/rpm maintainer scripts: `update-mime-database` / `update-desktop-database`. |
| `ado/` | Copied to `apps/desktop/src-tauri/binaries/ado` (untracked, next to the sidecar) by `cargo xtask dist stage`; the per-OS `bundle.resources` mapping carries it into the bundle as `<resource dir>/ado`. The tree `sysuse` resolves — see "The shipped ado tree" below. `ado/base/a/auto.dta` is a byte-identical copy of `tests/fixtures/dta/auto.dta` (the conformance oracle's input; `cargo xtask dist check` refuses drift, and the fixture is never edited — copy fixture → shipped, never the other direction). |
| `cross/` | `scripts/dev-setup.sh --cross` — zig-backed `x86_64-linux-gnu-*` compiler shims so `cargo check --target x86_64-unknown-linux-gnu` works from a macOS dev host; `cargo xtask dist cross-check` drives both cross triples (below). |

## Cross-target checks from a macOS dev host

```sh
./scripts/dev-setup.sh --cross      # once: the two triples' std + zig + shims
cargo xtask dist cross-check        # both triples; prints what it could not cover
```

`cargo check --target <triple>` runs every build script in the dependency
graph, and some of them compile C: `blake3` (a workspace dependency of half
the crates), `aws-lc-sys` (via `stratum-ai → reqwest → rustls`), `mimalloc`
(the desktop host's allocator). What a Mac can do about that differs per
triple, and `cargo xtask dist cross-check` (xtask/src/dist.rs) encodes it:

**`x86_64-unknown-linux-gnu` — full.** A stock Mac has no C compiler that can
target Linux/glibc, so `cc-rs` dies looking for a tool named
`x86_64-linux-gnu-gcc`. `dev-setup.sh --cross` installs zig (Homebrew) and
copies the `cross/` shims into `$(brew --prefix)/bin` under both names `cc-rs`
probes. zig bundles glibc headers and an assembler, so `aws-lc-sys`, blake3's
`.S` files and mimalloc compile fully — no `.cargo/config.toml` edits, no
per-invocation env vars, and a plain `cargo check --target
x86_64-unknown-linux-gnu` exits 0 (verified from a clean build of all three C
crates; ~30 s on an M-series machine).

**`x86_64-pc-windows-msvc` — PARTIAL, by the nature of the host.** Two
separate obstacles, and it matters which is which:

1. blake3 assumes MASM (`ml64.exe`) whenever it is cross-compiled to msvc
   with no `CC_x86_64_pc_windows_msvc` set — its build script treats "unset"
   as "cl.exe". Naming `clang` makes it take the GNU-syntax twins of its
   Windows assembly, which clang's integrated assembler emits as COFF without
   touching a header; any LLVM `lib`/`ar` driver archives the result
   (`zig lib`, which `--cross` already installed). `cross-check` sets exactly
   `CC_x86_64_pc_windows_msvc=clang` and `AR_x86_64_pc_windows_msvc=zig lib`
   (or `llvm-lib`/`llvm-ar` when they are on PATH) for that one invocation,
   and blake3 — hence every crate that uses it — type-checks for msvc.
2. `aws-lc-sys` and `libmimalloc-sys` `#include` the Windows CRT/SDK headers
   (`wchar.h`, `windows.h`, …). No compiler flag conjures those; only a Windows
   host has them. `cross-check` skips the members that reach those crates
   (today: `stratum-ai`, and `stratum-desktop` — which is not a default member
   anyway, see below), names each one with the reason, and reports
   `PARTIAL`. `--strict` turns that into a failure.

So from a Mac, a bare `cargo check --target x86_64-pc-windows-msvc` stops at
blake3 (`ml64.exe` not found) or, with the two env vars, at `aws-lc-sys`;
`cargo xtask dist cross-check` covers 21 of the 22 default members. Full
msvc coverage comes from a Windows host: CI builds and smokes that target
natively on `windows-2022` (`package.yml`, `smoke.yml`), which is the gate
that matters. The documented off-Windows route is
[cargo-xwin](https://github.com/rust-cross/cargo-xwin) (`cargo install
cargo-xwin`, `brew install llvm nasm`, `cargo xwin check --target
x86_64-pc-windows-msvc`): on first use it downloads Microsoft's CRT and
Windows SDK, whose license terms are yours to accept — which is why no script
here runs it and why it is not exercised by this repo.

**The desktop host is not in either check.** `apps/desktop/src-tauri` is a
workspace member but not a default member, so a bare `cargo check` never
builds it and `cross-check` lists it as out of scope. It cannot be
cross-checked from another OS in any case: Linux needs GTK/WebKitGTK/D-Bus
through pkg-config (`libdbus-sys` is the first to fail), Windows needs the
CRT via mimalloc. It is built and smoked natively per OS in CI.

## The shipped ado tree (sysuse)

The engine resolves `sysuse <name>` to a real file,
`<ado base>/<first-letter>/<name>.dta`, and prints the real path it loaded —
never a faked one. The base directory is found in this order:

1. **`STRATUM_ADO_BASE`** — an explicit environment override; always wins.
2. **Executable-adjacent** — an `ado/base` tree next to the engine's own
   executable (the dev-tree layout).

A packaged app satisfies neither on its own: the sidecar sits next to the host
executable (`Contents/MacOS` on macOS) while resources land in the per-OS
resource directory (`Contents/Resources` on macOS). So the desktop host
resolves its resource directory at startup and exports
`STRATUM_ADO_BASE=<resource dir>/ado/base` into the `stratum serve` child's
environment (`apps/desktop/src-tauri/src/main.rs`, the engine spawn) — unless
the user already set the variable, which is never overwritten. The plumbing:

- `packaging/ado/base/a/auto.dta` — the committed tree (one file today).
- `cargo xtask dist stage` — copies it to `src-tauri/binaries/ado`.
- `bundle.resources` in every `tauri.*.conf.json` per-OS config — maps
  `binaries/ado/base/a/auto.dta` to `ado/base/a/auto.dta` in the bundle.
  A new ado file needs a new mapping line in all three configs;
  `cargo xtask dist check` asserts the auto.dta mapping is present and the
  shipped bytes still equal the fixture.

## File associations: the one rule

Stata already declares itself the default handler for `.do`/`.dta` (measured,
spec §0). Stratum registers as a **capable alternative on every OS and never
steals the default**:

- macOS: `LSHandlerRank = Alternate`, UTIs **imported**, never exported.
- Windows: `OpenWithProgids` + Default Programs capabilities only; the
  `(Default)` value of `HKCU\Software\Classes\.do` is never written —
  `smoke.yml` asserts it is still null after install.
- Linux: MIME registration is additive by design.

## The unsigned era (ADR-011 §7.3)

Until the project holds an Apple Developer ID and an Azure Trusted Signing
account, releases are **ad-hoc signed on macOS (the `.app`; the `.dmg` is
deliberately unsigned, below) and unsigned on Windows**. The exact,
designed-not-discovered consequences:

- **macOS**: a quarantined (browser-downloaded) copy fails Gatekeeper with
  *"Stratum" is damaged and can't be opened. You should move it to the Trash.*
  Since macOS 15 right-click → Open no longer bypasses this. Recovery: attempt
  to open → dismiss → System Settings → Privacy & Security → "Open Anyway", or
  run `scripts/macos-unquarantine.sh`. `curl`-downloaded and `tar`-extracted
  copies carry no quarantine attribute and launch normally, so the pre-signing
  primary artifact is the `.app.tar.gz`; the `.dmg` is still produced and works
  for unquarantined copies.
- **macOS, the `.dmg` itself is unsigned in this era — by design, not
  omission.** Tauri's bundler skips disk-image signing for the ad-hoc identity
  `-` (tauri-bundler 2.2.3, PR #12323): Gatekeeper refuses an *ad-hoc-signed*
  `.dmg` at mount on macOS 15, before the user reaches the app and with no
  right-click → Open escape (issue #12288), whereas an unsigned image mounts
  and only the `.app` inside meets the consequence above. `cargo xtask sign
  verify-dmg` encodes this: in every era it runs `hdiutil verify` (the image's
  checksum) and refuses an ad-hoc-signed image; unsigned passes (with the
  reason printed) unless `--require-notarized`; Developer ID signed runs the
  hard gates — `codesign --verify --strict --deep`, `spctl -a -t open
  --context context:primary-signature` (Apple's assessment for disk images;
  `-t install` is for `.pkg`), `xcrun stapler validate`. Do **not** "fix" the
  unsigned image with `codesign -s -`: the gate will fail it, and users would
  be worse off.
- **Windows**: SmartScreen shows "Windows protected your PC"; the user must
  click **More info → Run anyway**. Signing late does not retroactively earn
  reputation — Developer ID + Trusted Signing are 1.0 release blockers.
- **Linux**: no OS gate; `SHA256SUMS` is GPG-signed from day one when
  `LINUX_GPG_PRIVATE_KEY` exists.

`package.yml` gates every signing/notarization step on its secrets existing;
the unsigned path is the default and produces launchable artifacts.

## Building a local release bundle

```sh
cargo build --release -p stratum-cli   # the externalBin sidecar
cargo run -p xtask -- dist stage       # sidecar + Info.plist into src-tauri/
cd apps/desktop
APPLE_SIGNING_IDENTITY="-" pnpm dlx @tauri-apps/cli@2.11.0 build
```

`APPLE_SIGNING_IDENTITY="-"` matters on macOS: it is codesign's ad-hoc
identity, and it makes the Tauri bundler sign the `.app` inside-out with the
committed entitlements (the `.dmg` stays unsigned on purpose — see the unsigned
era above). With the variable unset the bundler skips bundle-level signing
entirely and `codesign --verify --strict --deep` fails on the result. Verify
with — every line exits 0 on an ad-hoc build, and each prints what it asserted:

```sh
cargo run -p xtask -- dist verify --app target/release/bundle/macos/Stratum.app
cargo run -p xtask -- sign verify-app  target/release/bundle/macos/Stratum.app
cargo run -p xtask -- sign verify-dmg  target/release/bundle/dmg/Stratum_*.dmg
cargo run -p xtask -- smoke selftest   target/release/bundle/macos/Stratum.app
cargo run -p xtask -- smoke dmg        target/release/bundle/dmg/Stratum_*.dmg
```

`package.yml` runs the same `verify-app` + `verify-dmg` pair on the unsigned
path, and `--require-notarized` on both after notarize + staple on the signed
path; `smoke.yml` runs `verify-dmg` over the downloaded artifact on every run.
