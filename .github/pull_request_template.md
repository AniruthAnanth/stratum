## What

<!-- One paragraph: what changes and why. The PR TITLE must be a Conventional
     Commit (`type(scope): subject`) — it becomes the squashed commit on main
     and commitlint validates it. -->

## Verification

<!-- How you know it works: the tests you added or ran, the invariant checks
     that cover it. `cargo xtask layering`, `ownership`, and the rest of the
     ARCHITECTURE §8 gates run automatically. -->

## Trailers

<!-- Mandatory for changes under crates/ or apps/ (08 §11.2). Cite the
     PRODUCT_SPEC section(s) this implements, or `Spec: n/a` with a reason.
     CI warns rather than blocks. -->

Spec: §
Refs: #

## Checklist

- [ ] `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` are clean
- [ ] Tests pass locally (`cargo nextest run --workspace`)
- [ ] No file outside my work unit's ownership (docs/ownership.toml) is touched
- [ ] Goldens under `tests/difftest/cases/**/golden/**` and `tests/golden/**` are untouched (they are recorded oracle output — regeneration requires a documented Stata capture)
