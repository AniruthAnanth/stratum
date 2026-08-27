# `stratum-workspace-hack`

The `cargo-hakari` workspace-hack crate. It contains no code and never will.

## What it is for

Cargo unifies features per dependency **per build**, not per workspace. Building
`stratum-cli` selects one feature set for, say, `serde` and `indexmap`; building
`stratum-desktop` selects a different one; so switching between the two
recompiles the shared dependency and everything above it. On a workspace this
size that is worth several minutes per cold build
(`08-platform-packaging-ci.md` §10.5).

`cargo hakari` fixes it by computing the *union* of every feature set any member
selects, writing that union into this crate's `Cargo.toml`, and adding a
dependency on this crate to every other member. Every build then selects the
same union, so the shared dependencies are built once.

The generated `Cargo.toml` has to be committed (`08` §11.3): the unification
must be visible to cargo's resolver before the build starts, so it cannot be
produced by a build script.

## Current state: declared, not yet turned on

The `HAKARI SECTION` in `Cargo.toml` is empty, and no member depends on this
crate. That is the correct state today rather than a stub.

Hakari's entire job is to reconcile *conflicting* feature selections. W00 ships
`stratum-proto` and `stratum-tokens`; tokens has no dependencies at all, and
proto's four are selected by exactly one crate. There is nothing to unify.
Generating now would commit an artifact that is stale by the time it matters,
and `cargo hakari verify` would then start failing for a reason nobody caused.

## Turning it on

Do this once the workspace has enough members that two of them select different
feature sets for the same dependency — realistically once `stratum-cli` (W22)
and `stratum-desktop` (W17) both exist, since those are the two roots whose
trees diverge.

```sh
cargo install cargo-hakari
cargo hakari init          # writes .config/hakari.toml
cargo hakari generate      # fills the HAKARI SECTION in this crate's Cargo.toml
cargo hakari manage-deps   # adds `stratum-workspace-hack = { path = ... }` to every member
cargo hakari verify        # what CI runs
```

Three things to know before running it:

1. **`cargo hakari init` writes `.config/hakari.toml`, which no work unit
   currently owns.** `xtask ownership` (ARCHITECTURE §8.13) fails on any tracked
   file claimed by no unit, so that file has to be added to `docs/ownership.toml`
   in the same commit. Both files belong to W00.

2. **`cargo hakari manage-deps` edits every member's `Cargo.toml`.** Under R0
   those files have other owners. This is a workspace-wide mechanical edit, so
   it wants its own commit from whoever owns the manifest surface at the time —
   not a side effect of an unrelated change.

3. **Configure `.config/hakari.toml` to skip the crates that must stay lean.**
   `stratum-proto`, `stratum-core`, `stratum-parse`, `stratum-effects` and
   `stratum-intel` all build for `wasm32-unknown-unknown` and CI asserts their
   dependency trees reach no `std::fs`, `tokio`, `time`, `memmap2` or locale
   crate (ARCHITECTURE §8.4, ADR A2). A workspace-hack dependency pulls the
   *union* of the workspace's features into whatever depends on it, which would
   put `tokio` in `stratum-proto`'s tree and break invariant 4 on the first
   build. Those five belong in hakari's `[traversal-excludes]` /
   `final-excludes`, and the wasm target belongs in `platforms`.

   This is not hypothetical: it is the same failure A2 already had to fix once,
   when `time` was a proto dependency.

## `xtask layering` and this crate

`stratum-workspace-hack` is a workspace member and therefore matched by
`default-members = ["crates/*"]`. It is inert as long as its `HAKARI SECTION`
is empty. Once it is filled, it will legitimately depend on most of the
third-party graph, so any layering rule that walks dependency trees has to
exclude it by name — otherwise every crate that depends on it appears to reach
everything.
