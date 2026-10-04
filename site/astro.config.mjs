import { defineConfig } from "astro/config";
import { remarkRepoDocs } from "./src/lib/remark-repo-docs.mjs";

// GitHub Pages serves a project site under /<repo>/. `astro dev` serves from
// the root so the URL it prints opens the site.
const base = process.argv.includes("dev") ? "/" : "/daisy";

export default defineConfig({
  site: "https://misfitdev.github.io",
  base,
  trailingSlash: "always",
  markdown: {
    remarkPlugins: [[remarkRepoDocs, { base }]],
    shikiConfig: { theme: "css-variables" },
  },
});
