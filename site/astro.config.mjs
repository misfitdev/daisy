import { defineConfig } from "astro/config";
import { remarkRepoDocs } from "./src/lib/remark-repo-docs.mjs";

// GitHub Pages serves a project site under /<repo>/.
export default defineConfig({
  site: "https://misfitdev.github.io",
  base: "/daisy",
  trailingSlash: "always",
  markdown: {
    remarkPlugins: [remarkRepoDocs],
    shikiConfig: { theme: "css-variables" },
  },
});
