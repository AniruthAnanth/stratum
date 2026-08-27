#!/bin/sh
# Remove the com.apple.quarantine attribute from an installed Stratum.app
# (ADR-011 §7.3 — the unsigned era). A browser-downloaded, unnotarized app is
# refused by Gatekeeper with a misleading "damaged" dialog; removing the
# quarantine attribute tells macOS you accept the binary as-is. Only do this
# for a Stratum you downloaded from the project's own releases page, and
# ideally after verifying its entry in SHA256SUMS.
set -eu
APP="${1:-/Applications/Stratum.app}"
if [ ! -d "$APP" ]; then
  echo "macos-unquarantine: $APP not found (pass the path to Stratum.app)" >&2
  exit 1
fi
xattr -dr com.apple.quarantine "$APP"
echo "macos-unquarantine: quarantine removed from $APP"
