# Stratum contributor setup — Windows (design 08; W22). Idempotent.
$ErrorActionPreference = "Stop"

function Say($m) { Write-Host "`n== $m" }
function Need($c) { $null -ne (Get-Command $c -ErrorAction SilentlyContinue) }

Say "Rust toolchain (pinned by rust-toolchain.toml)"
if (-not (Need rustup)) {
  Write-Error "rustup is required: https://rustup.rs (use the x86_64-pc-windows-msvc host)"
}
rustup show active-toolchain; if ($LASTEXITCODE -ne 0) { rustup toolchain install }

Say "wasm target (W11a's stratum-wasm)"
rustup target add wasm32-unknown-unknown

Say "Visual Studio Build Tools"
if (-not (Test-Path "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe")) {
  Write-Warning "VS Build Tools (C++ workload) are required for the MSVC toolchain."
}

Say "Node + pnpm"
if (-not (Need node)) { Write-Error "Node >= 22.12 is required (winget install OpenJS.NodeJS.LTS)" }
if (-not (Need pnpm)) { corepack enable; corepack prepare pnpm@9.15.0 --activate }

Say "Frontend dependencies"
Push-Location (Join-Path $PSScriptRoot "..\apps\desktop")
pnpm install --frozen-lockfile
Pop-Location

Say "WebView2 runtime"
# Windows 11 ships it; Windows 10 needs the evergreen runtime once.
$wv2 = Get-ItemProperty -ErrorAction SilentlyContinue `
  "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
if (-not $wv2) { Write-Warning "WebView2 runtime not detected; the packaged app's bootstrapper installs it, dev builds need it now: https://developer.microsoft.com/microsoft-edge/webview2/" }

Say "cargo-nextest"
if (-not (Need cargo-nextest)) { cargo install cargo-nextest --locked }

Say "Done."
