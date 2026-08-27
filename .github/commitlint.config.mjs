// Stratum commitlint (design 08 §11.2, W22). Validated against the PR TITLE —
// we squash-merge, so the title becomes the commit subject on main.
export default {
  extends: ["@commitlint/config-conventional"],
  rules: {
    "type-enum": [
      2,
      "always",
      ["feat", "fix", "perf", "refactor", "test", "docs", "build", "ci", "chore", "revert"],
    ],
    "scope-enum": [
      2,
      "always",
      [
        // crate short names
        "proto", "core", "data", "dta", "parse", "effects", "intel", "runtime",
        "exec", "session", "stats", "graph", "ai", "workspace", "tokens",
        "wasm", "cli", "desktop", "platform", "difftest", "e2e",
        // cross-cutting
        "lint", "fmt", "packaging", "ci", "docs", "deps", "xtask",
      ],
    ],
    // A scope is strongly encouraged but a hotfix is never held hostage to one.
    "scope-empty": [1, "never"],
    "subject-case": [2, "never", ["start-case", "pascal-case", "upper-case"]],
    "header-max-length": [2, "always", 72],
    "body-max-line-length": [1, "always", 100],
  },
};
