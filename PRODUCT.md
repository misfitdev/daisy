# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

The product is macOS only; no other operating system is in scope. Impeccable has no macOS platform value, so `web` applies to the GitHub Pages site. The menu bar app (`LSUIElement`, macOS 26+, Apple silicon) follows the macOS Human Interface Guidelines and native AppKit/SwiftUI controls, not web or iOS conventions.

## Stack

- The existing application core is Rust and uses native macOS APIs.
- The native menu-bar controller and setup flow are implemented under the
  completed `mn-075` work.
- The public website is hosted with GitHub Pages. Its static-site tooling is
  delegated to the website implementation workflow.

## Users

Daisy is for people who use multiple Macs on the same network, commonly a
laptop and one or more desktop systems, and want one keyboard and trackpad to
move among them. They should not need to understand networking, cryptographic
keys, or terminal commands.

## Product Purpose

Daisy shares one Mac's keyboard, mouse, and trackpad gestures with other
Macs on the same network without an Apple ID, iCloud, or any other account. The
product vision is a workspace that can extend across as many Macs as practical
limits permit. Success means someone installs it, grants the required
permissions, pairs their Macs, and then moves among them by pushing the pointer
through screen edges, including Spaces, Mission Control, and
application-window swipes.

## Positioning

Daisy is Mac-to-Mac only by design. Both ends speak macOS natively, so keys,
clicks, and trackpad gestures pass through as themselves instead of being
translated through another operating system. Trust is established directly
between two Macs with a one-time pairing code, and each Mac controls how long
that trust lasts.

## Operating Context

- Macs are arranged around the driving Mac, and the user identifies which edge
  leads to each neighboring system.
- The Host owns the keyboard and trackpad; the Guest receives input. These are
  lightweight setup roles, not permanent device identities. Network connection
  direction and control direction are independent.
- First run requires Accessibility and Input Monitoring permission for
  `Daisy.app` on both Macs.
- During pairing, one Mac displays a code and the user enters it on the other.
- Control-Option-Command-Escape immediately returns control to the driving Mac.
- The current beta is app-first. Terminal commands remain available for
  diagnostics, automation and advanced networking.

## Capabilities and Constraints

- Working: pairing, encrypted reconnects, per-Mac trust policies with forced
  revocation, keyboard and mouse input across a shared edge, basic scrolling,
  Spaces, Mission Control and application-window swipes, shake-to-locate on
  macOS 27, and the recovery chord.
- Planned: Bonjour discovery. Automatic reconnection after sleep, wake and
  network changes is implemented; two-Mac verification remains. Clipboard sharing of text, rich text and images is implemented;
  two-Mac verification remains. Spatial positioning and chained connections
  among multiple Macs are future capabilities and must not be presented as
  working today.
- Capturing trackpad swipes requires macOS 27 on the driving Mac.
- Shake-to-locate still needs visual verification on a macOS 26 follower.
- Pairing codes are safe only as PAKE input. Never ask people to compare a code
  by eye as a security check.
- Both Macs enforce their own trust policy, so the stricter policy wins. When
  trust ends, the Macs must pair again.
- Current setup labels are **Host** and **Guest**. Host supplies input; Guest
  receives it. Keep this choice visually and conceptually subordinate: it is a
  session role, and positioned multi-Mac chains should make it less important
  over time. Technical internals may still use driving and following.
- Product terminology: **this system** for the local computer and **peer** for
  another computer. **Host** and **Guest** appear only in the role selector.
  Pair, trust, forget and recovery chord remain the user-facing action terms.

## Brand Commitments

- Name: Daisy, from the open-ended topology of a daisy chain.
- Voice: plain, direct, and concise; state what works and what does not without
  hype.
- The identity must portray seamless shared connectivity between macOS systems:
  separate Macs behaving like one continuous workspace.
- The visual system must scale beyond a pair toward an effectively unbounded
  fabric within practical limits. Do not reduce the identity to two devices
  joined by a single line or to a central hub with subordinate nodes.
- The primary mark is an open Daisy chain with lively, spatial movement. Links
  may enter or leave from any angle or edge, not only horizontally through the
  center.
- Connection state is visible in the menu bar using color alone: the flower
  center is yellow when connected and gray when disconnected. The menu-bar
  mark never gains chain links.
- Simplicity is visible and complexity is optional. The default experience must
  not imply configuration work or require the user to understand the product's
  many advanced options.
- Approved identity artwork includes the standalone flower mark, outlined Daisy
  wordmark, primary lockup and macOS app icon. Canonical web artwork lives in
  `site/public/brand/`; the packaged app icon is generated from
  `macos/DaisyIcon.svg`.

## Evidence on Hand

- `README.md` and `docs/` document usage, architecture, protocol, security,
  macOS internals, and releases.
- Official releases are Developer ID signed, notarized, and carry SLSA Build
  Level 3 provenance.
- An approved settings screenshot is available at
  `site/public/screenshots/daisy-setup.png`. No testimonials, press quotes,
  benchmarks, pricing or customer claims exist; future work must not fabricate
  them.
- The project is licensed under Apache-2.0 or MIT, at the user's option.

## Product Principles

1. Never require the Terminal. Setup, pairing, trust, and recovery must be
   available in the application.
2. Hide machinery without hiding guarantees. Present pair, trust, and forget
   in human terms without weakening the security model.
3. Show honest status. Make local, remote, waiting, and disconnected states
   explicit, with plain explanations and recovery actions.
4. Feel native. Input sharing should behave like a first-party macOS utility
   and disappear when it works.
5. Local control always wins. Recovery must be immediate and obvious.
6. Reveal complexity only on demand. A person who wants the simple path should
   never need to learn the advanced one.
