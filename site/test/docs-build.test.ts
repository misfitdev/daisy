import assert from "node:assert/strict";
import test from "node:test";
import { inspectDocs } from "../scripts/check-docs.mjs";

const document = (body: string) => `<main id="main"><h1>Guide</h1>${body}</main>`;

test("built docs accept relative links, fragments, assets and external links", () => {
  const pages = new Map([
    ["docs/index.html", document('<a href="/daisy/docs/user-guide/#keyboard">Keyboard</a><a href="/daisy/">Home</a><a href="https://example.org/">External</a>')],
    ["docs/user-guide/index.html", document('<h2 id="keyboard">Keyboard</h2><a href="#main">Top</a><img src="/daisy/screenshots/window.png" alt="Arrangement">')],
  ]);
  const result = inspectDocs(pages, new Set([...pages.keys(), "index.html", "screenshots/window.png"]));
  assert.equal(result.links, 4);
  assert.deepEqual(result.failures, []);
});

test("built docs reject broken paths, fragments, images and duplicate titles", () => {
  const pages = new Map([
    ["docs/index.html", document('<h1>Duplicate</h1><a href="/daisy/docs/missing/">Missing</a><a href="/daisy/docs/guide/#missing">Bad anchor</a><img src="/daisy/missing.png" alt="Missing">')],
    ["docs/guide/index.html", document('<h2 id="exists">Exists</h2>')],
  ]);
  const result = inspectDocs(pages, new Set(pages.keys()));
  assert.equal(result.failures.length, 4);
  assert.ok(result.failures.some((message) => message.includes("expected one page title")));
  assert.ok(result.failures.some((message) => message.includes("missing anchor")));
  assert.equal(result.failures.filter((message) => message.includes("missing destination")).length, 2);
});

test("built docs reject URLs outside the deployed project base", () => {
  const pages = new Map([["docs/index.html", document('<a href="/docs/guide/">Wrong base</a>')]]);
  assert.match(inspectDocs(pages, new Set(pages.keys())).failures[0], /outside site base/);
});
