---
version: 1
slug: "site-src-pages-index-astro"
primary_target: "site/src/pages/index.astro"
related_targets: ["site"]
---

## Scope

Daisy public site on GitHub Pages (Astro, `site/`). Landing is Persuade; docs, security, roadmap and download pages are Read. Issue mn-1pb.

Approved by the user 2026-09-26.

## Audience and job

Mac owners with two or more Macs on one desk, arriving cold from a link. Primary action: Download the 0.1.0 beta (`Daisy-0.1.0-macos-arm64.zip`). Proof: the virtual desk, an honest works/planned split, verifiable security (Noise, SPAKE2, notarization, SLSA L3).

## Constraints

- Written as if the rename (mn-66s) is done: Daisy, `Daisy.app`, `daisy` CLI.
- The menu-bar app and setup flow from `mn-075` are complete.
- Many Macs, spatial layout and chaining are planned (mn-pu5); anywhere the desk shows more than two Macs it says Planned.
- No invented screenshots, testimonials, benchmarks, pricing, counts.
- Desk demo is desktop and pointer only (user accepted); below desktop, on touch, or with reduced motion it becomes a static desk illustration. All other pages responsive, WCAG AA.
- The approved mark, wordmark and primary lockup live in
  `site/public/brand/`. The approved app screenshot lives in
  `site/public/screenshots/daisy-setup.png`, and the final social image is
  `site/public/og.png`. Flower and U-link diagrams remain code-native SVG
  following `DESIGN.md` geometry.
- Nothing outside `site/` and `.github/workflows/pages.yml`.

Build path: code-led; no image generation in this harness.

## Direction contract

THESIS: The site is a working Daisy. The visitor's own pointer crosses from one Mac to the next before they read a word of explanation; it refuses the product-page hero of a headline beside a screenshot.

OWN-WORLD: Daisy Paper field, graphite type, flat anodized-gray U links interlocking across display seams, yellow only in the center of the flower that has control, coral for one editorial phrase. Screen outlines, never device photography in the demo.

STORY: Visitor pushes the pointer off one screen and lands on the other, sees the flower light, understands "one keyboard, every Mac", sees plainly what works and what is planned, trusts it through checkable security, downloads.

FIRST VIEWPORT: Top band: nav, headline "One keyboard and trackpad. Every Mac." with Download 0.1.0 beside it. Below, a hint row with the one-line hint "Push the pointer through the edge." and the controls (arrange displays, Spaces swipe, disconnect, Add a Mac (Planned), reset), kept above the desk so both land in the first viewport. Then the desk fills the remaining height, centered and scaled to fit (it keeps its aspect so links never stretch, which leaves side margin on wide screens): two screen outlines each with a centered flower, interlocking links across the shared seam, keyboard and trackpad in front of the driving Mac, the visitor's pointer live inside the left screen. The Esc line sits under the desk.

FORM: The Long Desk, dealt structure 2 of the ordered list, pushed to overdrive as an interactive virtual Daisy by the user's steer. Seed key 2cfba0b1.

SIGNATURE INTERACTION: Edge crossing. Local pointer pins and hides at the seam, the remote screen gains a pointer at the proportional position, the chain tugs from source to target and the target flower blooms. Yellow centers mean connected, per DESIGN.md; disconnecting turns them gray and drops the chain. Holding the mouse button blocks the crossing, as the real product does.

FINISH: reviewed and documented; every shipping raster has an approved source in
the repository.
