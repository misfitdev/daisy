// Repository docs rendered on the site, keyed by file name in ../docs.
export const docs = [
  { file: "usage", slug: "user-guide", title: "User guide", group: "Use", lede: "Everything the app does, trust and data, troubleshooting, and the optional command line." },
  { file: "administration", slug: "administration", title: "Administration", group: "Use", lede: "Settings an organization can enforce with a configuration profile." },
  { file: "architecture", slug: "how-it-works", title: "How it works", group: "Understand", lede: "Control and layout, the layers, and what happens at the edge." },
  { file: "security-model", slug: "security-model", title: "Security model", group: "Understand", lede: "Security controls, trust boundaries, and key handling." },
  { file: "protocol", slug: "protocol", title: "Protocol", group: "Understand", lede: "The wire format and the session, step by step." },
  { file: "macos", slug: "macos-internals", title: "macOS internals", group: "Understand", lede: "Permissions, the event tap, pointer pinning and swipes." },
  { file: "releasing", slug: "releasing", title: "Releasing", group: "Contribute", lede: "Signing, notarization and verifying a release." },
];

export const slugFor = Object.fromEntries(docs.map((d) => [d.file, d.slug]));
export const titleFor = Object.fromEntries(docs.map((d) => [d.file, d.title]));
