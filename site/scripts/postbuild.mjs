// Finish the static export for GitHub Pages.
//
// `.nojekyll` stops Pages running Jekyll over out/, which would otherwise drop
// the `_next/` directory (Jekyll ignores underscore-prefixed paths). Then a
// cheap sanity pass: the export exists and every asset is under /stratum/.

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const out = resolve(import.meta.dirname, "..", "out");
const index = resolve(out, "index.html");

if (!existsSync(index)) {
  console.error(`postbuild: ${index} missing — did \`next build\` export?`);
  process.exit(1);
}

writeFileSync(resolve(out, ".nojekyll"), "");

const html = readFileSync(index, "utf8");
const bare = html.match(/(?:src|href)="\/_next\//g);
if (bare) {
  console.error(`postbuild: ${bare.length} asset URL(s) missing the /stratum basePath`);
  process.exit(1);
}
if (!html.includes("/stratum/_next/")) {
  console.error("postbuild: no /stratum/_next/ asset URLs in index.html — basePath lost?");
  process.exit(1);
}

console.log("postbuild: out/.nojekyll written; assets resolve under /stratum/");
