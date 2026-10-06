import assert from "node:assert/strict";
import test from "node:test";
import { existsSync } from "node:fs";
import { remarkRepoDocs } from "../src/lib/remark-repo-docs.mjs";
import { docs, groups } from "../src/lib/docs.mjs";

function renderedLinks(path: string, urls: string[]) {
  const tree = {
    type: "root",
    children: [
      { type: "heading", depth: 1, children: [{ type: "text", value: "Title" }] },
      ...urls.map((url) => ({ type: "link", url, children: [{ type: "text", value: url }] })),
    ],
  };
  remarkRepoDocs({ base: "/daisy" })(tree, { path });
  assert.equal(tree.children.some((node) => node.type === "heading"), false);
  return tree.children.map((node) => (node as { url?: string }).url);
}

test("contributor navigation uses the canonical repository document", () => {
  const entry = docs.find((doc) => doc.slug === "contributing");
  assert.equal(entry?.file, "CONTRIBUTING");
  assert.equal(entry?.group, "Contribute");
});

test("contributor links resolve to site guides and repository instructions", () => {
  assert.deepEqual(renderedLinks("/repo/CONTRIBUTING.md", [
    "docs/usage.md#troubleshooting", "docs/", "AGENTS.md", "SECURITY.md",
  ]), [
    "/daisy/docs/user-guide/#troubleshooting", "/daisy/docs/",
    "https://github.com/misfitdev/daisy/blob/main/AGENTS.md", "/daisy/security/",
  ]);
});

test("reference links preserve anchors and reach the contributor guide", () => {
  assert.deepEqual(renderedLinks("/repo/docs/releasing.md", [
    "usage.md#homebrew", "../CONTRIBUTING.md", "../SECURITY.md", "#local", "https://example.org/",
  ]), [
    "/daisy/docs/user-guide/#homebrew", "/daisy/docs/contributing/", "/daisy/security/",
    "#local", "https://example.org/",
  ]);
});


test("setup, trust and troubleshooting use canonical Markdown routes", () => {
  for (const slug of ["getting-started", "trust", "troubleshooting"]) {
    assert.equal(docs.find((doc) => doc.slug === slug)?.file, slug);
    assert.equal(existsSync(new URL(`../src/pages/docs/${slug}.astro`, import.meta.url)), false,
      "A handwritten route must not override canonical Markdown");
  }
  assert.deepEqual(groups.flatMap((group) => group.items),
    groups.flatMap((group) => docs.filter((doc) => doc.group === group.name)));
  assert.equal(new Set(groups.flatMap((group) => group.items.map((doc) => doc.slug))).size, docs.length);
});

test("task-index links resolve to the same site directory", () => {
  assert.deepEqual(renderedLinks("/repo/docs/usage.md", ["README.md"]), ["/daisy/docs/"]);
  assert.deepEqual(renderedLinks("/repo/CONTRIBUTING.md", ["docs/README.md"]), ["/daisy/docs/"]);
});

test("repository screenshots resolve to site assets", () => {
  const tree = { type: "root", children: [
    { type: "image", url: "../site/public/screenshots/daisy-window.png", alt: "Arrangement" },
  ] };
  remarkRepoDocs({ base: "/daisy" })(tree, { path: "/repo/docs/getting-started.md" });
  assert.equal(tree.children[0].url, "/daisy/screenshots/daisy-window.png");
});

test("website downloads use the configured release while GitHub remains evergreen", () => {
  function download(downloadUrl?: string) {
    const tree = { type: "root", children: [
      { type: "link", url: "https://github.com/misfitdev/daisy/releases/latest", children: [{ type: "text", value: "Download Daisy" }] },
    ] };
    remarkRepoDocs({ base: "/daisy", downloadUrl })(tree, { path: "/repo/docs/getting-started.md" });
    return tree.children[0].url;
  }
  const asset = "https://github.com/misfitdev/daisy/releases/download/v1.2.3/Daisy-1.2.3-macos-arm64.dmg";
  assert.equal(download(asset), asset);
  assert.equal(download(), "https://github.com/misfitdev/daisy/releases/latest");
});
