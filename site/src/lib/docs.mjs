// Repository docs rendered on the site, keyed by file name in ../docs.
export const docs = [
  { file: "getting-started", slug: "getting-started", title: "Getting started", group: "Start", lede: "Install. Start. Pair." },
  { file: "trust", slug: "trust", title: "Manage trust", group: "Use", lede: "Choose how long peers can connect and what happens when you forget one." },
  { file: "troubleshooting", slug: "troubleshooting", title: "Fix a problem", group: "Troubleshoot", lede: "Find the symptom, check the cause and take the next step." },
  { file: "usage", slug: "user-guide", title: "User guide", group: "Use", lede: "Set up, use and troubleshoot Daisy; find advanced tasks when you need them." },
  { file: "administration", slug: "administration", title: "Administration", group: "Use", lede: "Settings an organization can enforce with a configuration profile." },
  { file: "architecture", slug: "how-it-works", title: "How it works", group: "Understand", lede: "Control and layout, the layers, and what happens at the edge." },
  { file: "security-model", slug: "security-model", title: "Security model", group: "Understand", lede: "Security controls, trust boundaries, and key handling." },
  { file: "protocol", slug: "protocol", title: "Protocol", group: "Understand", lede: "The wire format and the session, step by step." },
  { file: "macos", slug: "macos-internals", title: "macOS internals", group: "Understand", lede: "Permissions, the event tap, pointer pinning and swipes." },
  { file: "CONTRIBUTING", slug: "contributing", title: "Contributing", group: "Contribute", lede: "Set up a development build, test a change and submit it for review." },
  { file: "releasing", slug: "releasing", title: "Releasing", group: "Contribute", lede: "Signing, notarization and verifying a release." },
];

export const groups = ["Start", "Use", "Troubleshoot", "Understand", "Contribute"].map((name) => ({
  name, items: docs.filter((doc) => doc.group === name),
}));

export const slugFor = Object.fromEntries(docs.map((d) => [d.file, d.slug]));
export const titleFor = Object.fromEntries(docs.map((d) => [d.file, d.title]));
