import { visit } from "unist-util-visit";
import { slugFor, titleFor } from "./docs.mjs";

const repo = "https://github.com/misfitdev/daisy";

// Links between repository docs point at site routes; other repository paths
// point at GitHub.
function rewrite(url, base, downloadUrl) {
  if (downloadUrl && url === `${repo}/releases/latest`) return downloadUrl;
  if (/^[a-z]+:/i.test(url) || url.startsWith("#")) return url;
  const [path, hash = ""] = url.split("#");
  const anchor = hash ? `#${hash}` : "";
  if (path.startsWith("../site/public/")) return `${base}/${path.slice("../site/public/".length)}${anchor}`;
  if (path === "README.md" || path === "docs/README.md") return `${base}/docs/${anchor}`;
  const doc = path.match(/^(?:\.\/|docs\/|\.\.\/)?([\w-]+)\.md$/);
  if (doc && slugFor[doc[1]]) return `${base}/docs/${slugFor[doc[1]]}/${anchor}`;
  if (path === "../SECURITY.md" || path === "SECURITY.md") return `${base}/security/${anchor}`;
  if (path === "AGENTS.md") return `${repo}/blob/main/AGENTS.md${anchor}`;
  if (path === "docs/") return `${base}/docs/`;
  if (path.startsWith("../")) return `${repo}/blob/main/${path.slice(3)}${anchor}`;
  return url;
}

// A link whose text is a bare file name ("protocol.md") reads as the page title on the site.
function retitle(link) {
  if (link.children.length !== 1) return;
  const text = link.children[0];
  if (text.type !== "text" && text.type !== "inlineCode") return;
  const name = text.value.match(/^(?:\.\.\/)?([\w-]+)\.md$/);
  if (!name) return;
  const title = name[1] === "SECURITY" ? "Security" : titleFor[name[1]];
  if (title) link.children = [{ type: "text", value: title }];
}

/** Drops each repository doc's H1 (the page supplies the title) and rewrites its links. */
export function remarkRepoDocs({ base = "/daisy", downloadUrl } = {}) {
  const root = base.replace(/\/$/, "");
  return (tree, file) => {
    const isRepoDoc = String(file.path ?? file.history?.[0] ?? "").match(/\/docs\/|\/CONTRIBUTING\.md$/);
    if (!isRepoDoc) return;
    visit(tree, (node, index, parent) => {
      if (node.type === "heading" && node.depth === 1 && parent && index !== undefined) {
        parent.children.splice(index, 1);
        return index;
      }
      if (node.type === "image") node.url = rewrite(node.url, root);
      if (node.type === "link") {
        node.url = rewrite(node.url, root, downloadUrl);
        retitle(node);
      }
    });
  };
}
