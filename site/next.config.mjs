// Stratum landing page — static export served by GitHub Pages from the
// gh-pages branch of AniruthAnanth/stratum, under https://aniruthananth.github.io/stratum/.
//
// `basePath` is what makes every asset resolve as /stratum/_next/... on Pages;
// `trailingSlash` makes `/stratum/` serve out/index.html without a redirect;
// `images.unoptimized` is required for `output: 'export'` (no image server).

/** @type {import('next').NextConfig} */
const nextConfig = {
  output: "export",
  basePath: "/stratum",
  trailingSlash: true,
  images: { unoptimized: true },
};

export default nextConfig;
