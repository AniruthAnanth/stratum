#!/usr/bin/env bash
# Build the PUBLIC tree: the product, with planning and process material
# removed. Machine-read inputs stay, whatever directory they live in.
#
#   scripts/publish-tree.sh <dest-dir> [rev]    (rev defaults to HEAD)
#
# Exits non-zero, and removes the tree, if the result would leak anything.
set -euo pipefail
dest="${1:?dest dir}"
rev="${2:-HEAD}"
rm -rf "$dest"; mkdir -p "$dest"
# Export the COMMIT, not the working tree. Publishing working-tree files once
# shipped an agent's half-finished edit of main.rs - helpers defined, not yet
# wired - and the public lint job failed on "never used". What is public must
# be a thing git can name.
git archive --format=tar "$rev" | tar -x -C "$dest"
( cd "$dest" && find docs -type f ! -name ownership.toml -delete 2>/dev/null; rm -f DECISIONS.md; rmdir docs 2>/dev/null || true )

# Refusal gate. Patterns are GENERIC on purpose: an unredacted Stata licence
# banner has a numeric serial and a named licensee; matching the shape catches
# any future capture without this script itself carrying the values.
leak=0
if grep -rnE "^Serial number: *[0-9]{6,}|^ *Licensed to: *[A-Z][a-z]+ [A-Z]" "$dest" --include='*.log' --include='*.txt' --include='*.md' >/dev/null 2>&1; then
  echo "publish-tree: an unredacted Stata licence banner is in the tree; refusing" >&2; leak=1
fi
if grep -rl "stratum\.dev" "$dest" --include='*.xml' --include='*.json' --include='*.toml' --include='*.yml' --include='*.rs' --include='*.ts' --include='*.tsx' --include='*.plist' >/dev/null 2>&1; then
  echo "publish-tree: unowned domain stratum.dev referenced in the tree; refusing" >&2; leak=1
fi
if [ "$leak" = 1 ]; then rm -rf "$dest"; exit 1; fi
echo "publish-tree: $(find "$dest" -type f | wc -l | tr -d ' ') files -> $dest"
