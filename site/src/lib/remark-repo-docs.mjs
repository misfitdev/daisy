import { visit } from "unist-util-visit";
import { slugFor } from "./docs.mjs";

const base = "/daisy";
const repo = "https://github.com/misfitdev/daisy";

// Links between repository docs point at site routes; other repository paths
// point at GitHub.
function rewrite(url) {
  if (/^[a-z]+:/i.test(url) || url.startsWith("#")) return url;
  const [path, hash = ""] = url.split("#");
  const anchor = hash ? `#${hash}` : "";
  const doc = path.match(/^(?:\.\/)?([\w-]+)\.md$/);
  if (doc && slugFor[doc[1]]) return `${base}/docs/${slugFor[doc[1]]}/${anchor}`;
  if (path === "../SECURITY.md") return `${base}/security/${anchor}`;
  if (path.startsWith("../")) return `${repo}/blob/main/${path.slice(3)}${anchor}`;
  return url;
}

/** Drops each repository doc's H1 (the page supplies the title) and rewrites its links. */
export function remarkRepoDocs() {
  return (tree, file) => {
    const isRepoDoc = String(file.path ?? file.history?.[0] ?? "").includes("/docs/");
    if (!isRepoDoc) return;
    visit(tree, (node, index, parent) => {
      if (node.type === "heading" && node.depth === 1 && parent && index !== undefined) {
        parent.children.splice(index, 1);
        return index;
      }
      if (node.type === "link") node.url = rewrite(node.url);
    });
  };
}
