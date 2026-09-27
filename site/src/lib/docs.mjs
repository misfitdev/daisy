// Repository docs rendered on the site, keyed by file name in ../docs.
export const docs = [
  { file: "usage", slug: "user-guide", title: "User guide", group: "Use" },
  { file: "architecture", slug: "how-it-works", title: "How it works", group: "Understand" },
  { file: "security-model", slug: "security-model", title: "Security model", group: "Understand" },
  { file: "protocol", slug: "protocol", title: "Protocol", group: "Understand" },
  { file: "macos", slug: "macos-internals", title: "macOS internals", group: "Understand" },
  { file: "releasing", slug: "releasing", title: "Releasing", group: "Contribute" },
];

export const slugFor = Object.fromEntries(docs.map((d) => [d.file, d.slug]));
