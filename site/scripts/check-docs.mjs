import { readdirSync, readFileSync } from "node:fs";
import { resolve, relative } from "node:path";
import { pathToFileURL } from "node:url";
import { docs } from "../src/lib/docs.mjs";

/** Check emitted documentation links and images, including local fragments. */
export function inspectDocs(pages, files, base = "/daisy") {
  const failures = [];
  let links = 0;
  const origin = "https://docs.invalid";
  const ids = new Map([...pages].map(([path, html]) => [path,
    new Set([...html.matchAll(/\bid="([^"]+)"/g)].map((match) => match[1])),
  ]));
  for (const [path, html] of pages) {
    if (!path.startsWith("docs/")) continue;
    if ([...html.matchAll(/<h1(?:\s|>)/g)].length !== 1) {
      failures.push(`${path}: expected one page title`);
    }
    for (const match of html.matchAll(/<(?:a|img)\b[^>]*\b(?:href|src)="([^"]+)"/g)) {
      const href = match[1].replaceAll("&amp;", "&");
      const url = new URL(href, `${origin}${base}/${path}`);
      if (url.origin !== origin) continue;
      links += 1;
      if (!url.pathname.startsWith(`${base}/`)) {
        failures.push(`${path}: outside site base: ${href}`);
        continue;
      }
      let target = decodeURIComponent(url.pathname.slice(base.length + 1));
      if (!target || target.endsWith("/")) target += "index.html";
      if (!files.has(target)) {
        failures.push(`${path}: missing destination: ${href}`);
      } else if (url.hash && ids.has(target) && !ids.get(target).has(decodeURIComponent(url.hash.slice(1)))) {
        failures.push(`${path}: missing anchor: ${href}`);
      }
    }
  }
  return { links, failures };
}

export function checkBuiltDocs(root) {
  const files = new Set();
  const pages = new Map();
  function walk(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) walk(path);
      else {
        const name = relative(root, path).replaceAll("\\", "/");
        files.add(name);
        if (name.endsWith(".html")) pages.set(name, readFileSync(path, "utf8"));
      }
    }
  }
  walk(root);
  const result = inspectDocs(pages, files);
  for (const slug of ["", ...docs.map((doc) => `${doc.slug}/`)]) {
    if (!pages.has(`docs/${slug}index.html`)) result.failures.push(`Missing documentation page: ${slug}`);
  }
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const result = checkBuiltDocs(resolve(import.meta.dirname, "../dist"));
  if (result.failures.length) {
    console.error(result.failures.join("\n"));
    process.exitCode = 1;
  } else console.log(`Documentation: ${result.links} local links and images checked; no missing destinations or anchors.`);
}
