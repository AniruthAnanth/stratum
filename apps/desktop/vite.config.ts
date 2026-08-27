/// <reference types="vitest/config" />
import { cpSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { type Plugin, defineConfig } from "vite";
import solid from "vite-plugin-solid";

/**
 * 06 §1 — Vite 6, SolidJS, plain CSS + CSS Modules. No Tailwind, no CSS-in-JS.
 *
 * Two entries, one bundle (06 §13.3): `index.html` is the main window and
 * `pane.html` is the detached-pane shell. They share every chunk, so a detached
 * pane opens against a warm module graph instead of downloading a second copy
 * of Solid, the dock and the keyboard trie.
 */
const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));

/**
 * W11a's wasm glue is reached through `import(/* @vite-ignore *\/ "./generated/
 * stratum_wasm.js")` (`src/wasm/loader.ts`), which Rollup deliberately does not
 * follow — so the module graph never carries the glue or the `.wasm` into
 * `dist`, and the import resolves at RUNTIME relative to the importing chunk in
 * `dist/assets/`. In dev, Vite serves the source tree and it just works; in the
 * packaged app it was a 404 and an unsegmented editor. Copy the generated
 * directory to where the runtime path points. Registered by W17 (this file is
 * W12's): the packaged host is where the defect is observable.
 */
const copyWasmGenerated = (): Plugin => ({
  name: "stratum-copy-wasm-generated",
  apply: "build",
  writeBundle() {
    cpSync(here("./src/wasm/generated"), here("./dist/assets/generated"), { recursive: true });
  },
});

export default defineConfig(({ mode }) => ({
  plugins: [solid(), copyWasmGenerated()],

  // W11a's stub fence. `src/wasm/loader.ts` reaches the development stub only
  // from inside `if (STUB_ALLOWED)`; folding this to the literal `false` makes
  // that branch unreachable and Rollup drops the whole `src/wasm/stub/**` subtree
  // with it. `cargo xtask wasm --check-bundle dist` greps the emitted assets for
  // the stub's sentinel and fails the build if any of it survived.
  //
  // It must be a literal, which is why this config took the function form: a
  // define whose value is itself `import.meta.env.DEV` is substituted once and
  // never folded, so the branch stays live and the stub ships. Under vitest
  // (mode "test") it is `true`, which is what `src/wasm/conformance.test.ts`
  // drives both ways. Registered here by W11a; this file is W12's.
  define: {
    __STRATUM_ALLOW_WASM_STUB__: JSON.stringify(mode !== "production"),
  },

  // Tauri serves the built bundle from a custom scheme; relative asset URLs are
  // the only form that resolves identically under `stratum-asset://localhost/app/`
  // and under the dev server.
  base: "./",

  clearScreen: false,
  envPrefix: ["VITE_", "STRATUM_"],

  server: {
    // 08's Tauri dev config points at this port; changing it breaks `tauri dev`.
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },

  build: {
    target: "es2022",
    // No sourcemap-stripping in debug builds: a stack trace from a packaged
    // beta is worth more than the bytes.
    sourcemap: true,
    rollupOptions: {
      input: { main: here("./index.html"), pane: here("./pane.html") },
    },
  },

  css: {
    modules: {
      // Readable in the inspector, stable across builds, still unique.
      generateScopedName: "[name]_[local]_[hash:base64:4]",
    },
  },

  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    setupFiles: [here("./src/platform/shims.ts")],
    // vite-plugin-solid needs the `development` export condition under test or
    // Solid's dev-only reactivity warnings never load.
    server: { deps: { inline: [/solid-js/] } },
    coverage: {
      provider: "v8",
      include: ["src/**/*.ts", "src/**/*.tsx"],
      exclude: [
        "src/**/*.test.ts",
        "src/**/*.test.tsx",
        "src/ipc/commands.ts",
        "src/ipc/events.ts",
        "src/ipc/types.ts",
      ],
      reporter: ["text", "lcov"],
    },
  },

  resolve: {
    conditions: ["development", "browser"],
  },
}));
