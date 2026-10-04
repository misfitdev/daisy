import { readFileSync } from "node:fs";
import { defineConfig } from "astro/config";
import { remarkRepoDocs } from "./src/lib/remark-repo-docs.mjs";

// CI sets DAISY_VERSION to the latest published release so the download link
// never points at a DMG that does not exist yet; local builds use Cargo.toml.
const version =
  process.env.DAISY_VERSION ||
  readFileSync(new URL("../Cargo.toml", import.meta.url), "utf8").match(/^version = "([^"]+)"/m)[1];

// GitHub Pages serves a project site under /<repo>/. `astro dev` serves from
// the root so the URL it prints opens the site.
const base = process.argv.includes("dev") ? "/" : "/daisy";

export default defineConfig({
  site: "https://misfitdev.github.io",
  base,
  trailingSlash: "always",
  vite: { define: { "import.meta.env.DAISY_VERSION": JSON.stringify(version) } },
  markdown: {
    remarkPlugins: [[remarkRepoDocs, { base }]],
    shikiConfig: { theme: "css-variables" },
  },
});
