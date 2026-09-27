---
name: Daisy
description: A quiet, native visual system for one continuous workspace across many Macs.
colors:
  paper: "#F7F6F2"
  paper-raised: "#EFEEE9"
  paper-pressed: "#E6E5DF"
  surface-white: "#FFFFFF"
  rule: "#DDDCD5"
  graphite: "#2E3235"
  secondary-ink: "#4F565B"
  code-muted: "#6A7379"
  anodized-gray: "#737D84"
  connection-yellow: "#FFD447"
  coral-annotation: "#FF6B5E"
  disconnected-gray: "#A3A7AA"
typography:
  display:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'SF Pro Display', 'Helvetica Neue', sans-serif"
    fontSize: "clamp(2.5rem, 1.2rem + 3.2vw, 4.5rem)"
    fontWeight: 600
    lineHeight: 1.02
    letterSpacing: "-0.03em"
  headline:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'SF Pro Display', 'Helvetica Neue', sans-serif"
    fontSize: "clamp(1.75rem, 1.25rem + 1.6vw, 2.75rem)"
    fontWeight: 600
    lineHeight: 1.1
    letterSpacing: "-0.02em"
  body:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'SF Pro Text', 'Helvetica Neue', sans-serif"
    fontSize: "1.0625rem"
    fontWeight: 400
    lineHeight: 1.5
  label:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'SF Pro Text', 'Helvetica Neue', sans-serif"
    fontSize: "0.9375rem"
    fontWeight: 500
    lineHeight: 1.2
  mono:
    fontFamily: "ui-monospace, 'SF Mono', Menlo, monospace"
    fontSize: "0.875rem"
    lineHeight: 1.6
rounded:
  sm: "6px"
  md: "12px"
  lg: "14px"
  pill: "999px"
spacing:
  "1": "0.5rem"
  "2": "1rem"
  "3": "1.5rem"
  "4": "2.5rem"
  "5": "4rem"
  "6": "clamp(5rem, 3rem + 6vw, 9rem)"
components:
  button-primary:
    backgroundColor: "{colors.graphite}"
    textColor: "{colors.paper}"
    rounded: "{rounded.pill}"
    padding: "0.85rem 1.35rem"
  button-primary-hover:
    backgroundColor: "#1D2023"
  button-ghost:
    backgroundColor: "transparent"
    textColor: "{colors.graphite}"
    rounded: "{rounded.pill}"
    padding: "0.85rem 1.35rem"
  button-ghost-hover:
    backgroundColor: "{colors.paper-raised}"
  chip-planned:
    backgroundColor: "{colors.graphite}"
    textColor: "{colors.paper}"
    rounded: "{rounded.pill}"
    padding: "0.25em 0.55em"
  code-block:
    backgroundColor: "{colors.surface-white}"
    textColor: "{colors.graphite}"
    typography: "{typography.mono}"
    rounded: "{rounded.md}"
    padding: "1.1rem 1.25rem"
  placeholder:
    backgroundColor: "{colors.paper-raised}"
    textColor: "{colors.secondary-ink}"
    rounded: "{rounded.lg}"
    padding: "{spacing.3}"
---

# Design System: Daisy

## 1. Overview

### Creative North Star: "The Open Daisy Chain"

Daisy makes separate Macs feel like one continuous workspace. Its identity pairs a
simple centered flower with repeating, interlocking U-shaped links. The flower is a
Mac in the chain; repetition implies that another Mac can always join. The system
must never collapse into a two-computer cable, a central hub, or a topology diagram
that asks the viewer to understand networking.

The visual field is quiet, warm, and native to macOS. Graphite flowers and matte
anodized-gray links sit on warm off-white. Connection yellow is a precise state
signal, not a decorative wash. Coral supplies occasional editorial liveliness but
never competes with status.

The approved direction is recorded in the app icon at `macos/DaisyIcon.svg` and
the web identity assets under `site/public/brand/`. Their geometry and state
semantics are binding. Petal contour, optical spacing and exact wordmark drawing
may be refined during production without changing those rules. The website's
vector marks in `site/src/lib/marks.ts` follow the same rules.

**Key Characteristics:**

- A centered Daisy node gives every Mac equal visual weight.
- Repeating fixed-size U links make the chain feel open-ended.
- Matte metal gray keeps connectivity present without making it loud.
- Yellow means connected; gray means disconnected.
- Complexity appears only when it helps explain the product.

## 2. Colors

The palette is nearly neutral. Color is scarce enough that connection status reads
immediately.

### Primary

- **Daisy Paper** (`#F7F6F2`): the warm off-white identity field and preferred
  editorial background.
- **Graphite Petal** (`#2E3235`): primary lettering, high-emphasis text, flower
  petals when contrast permits, primary buttons, and drawn display bezels.

### Secondary

- **Connection Yellow** (`#FFD447`): the center of a connected flower. It is not a
  background, link color, or general-purpose highlight.
- **Coral Annotation** (`#FF6B5E`): restrained editorial emphasis and the native
  app's primary interaction accent. It never communicates connection state.

### Neutral

- **Anodized Link** (`#737D84`): every chain link, the normal neutral icon
  material, and non-text marks such as list bullets, quote rules, and
  arranging-mode outlines. It is visibly lighter than black and remains flat, cool,
  and matte.
- **Disconnected Gray** (`#A3A7AA`): the flower center and reduced-emphasis details
  in a disconnected state.
- **Secondary Ink** (`#4F565B`): secondary body text, ledes, captions, meta lines,
  inactive navigation, and code strings and constants.
- **Code Muted** (`#6A7379`): code comments and punctuation, on white code surfaces
  only.
- **Paper Raised** (`#EFEEE9`) and **Paper Pressed** (`#E6E5DF`): tonal steps above
  Daisy Paper for hover fills, inline code, notices, placeholders, the current
  docs-nav item, and step counters.
- **Surface White** (`#FFFFFF`): code blocks, keycaps, and desk controls on paper.
- **Rule** (`#DDDCD5`): 1px hairlines between sections, list rows, table rows, and
  the footer.

### Named Color Rules

**The Yellow Signal Rule.** Yellow appears only at the center of a connected Daisy.

**The Matte Metal Rule.** U links use one flat anodized-gray value. Never add sheen,
highlight, gradient, gloss, or simulated brushed-metal texture.

**The Coral Restraint Rule.** Coral marks editorial emphasis or the current primary
interaction. It is never a link, field background, permission state, warning, or
connection-status color.

**The Coral Underline Rule.** Coral never fills text: it measures 2.6:1 on Daisy
Paper. Emphasize at most one display phrase per page with a thick coral underline
(thickness `0.09em`, offset `0.12em`, skip-ink off) beneath graphite text.

**The Secondary Ink Rule.** Secondary text is Secondary Ink (6.9:1 on paper), never
Anodized Link, which measures 3.9:1 and fails AA for body text. Anodized gray is for
links, strokes, and marks.

**The Monochrome Code Rule.** Code highlighting uses no hue: keywords, functions,
parameters, and plain text in graphite; strings and constants in Secondary Ink;
comments and punctuation in Code Muted (4.8:1 on white, 4.5:1 on paper, so valid
only on the white code surface).

## 3. Typography

**Display Font:** SF Pro Display through the macOS system stack

**Body Font:** SF Pro Text through the macOS system stack

**Mono Font:** SF Mono through `ui-monospace`

**Character:** Plain, direct, and native. Type supports the mark rather than
performing a second visual concept. The Daisy wordmark is custom artwork; it must not
be reconstructed by typing the name in a nearby font. Canonical web artwork is
`site/public/brand/daisy-mark.svg`, `daisy-wordmark.svg` and
`daisy-lockup.svg`; `macos/DaisyIcon.svg` is the canonical app-icon source.

### Hierarchy

- **Display** (semibold, tight `-0.03em` tracking, `1.02` line height): short brand
  statements and major product moments, such as the home headline. Inner-page titles
  use the same voice one step smaller (`clamp(2.25rem, 1.5rem + 2.6vw, 3.75rem)`,
  `1.04` line height).
- **Headline** (semibold, `-0.02em` tracking, `1.1` line height): section-level
  communication and closing statements.
- **Title** (semibold, `clamp(1.2rem, 1.05rem + 0.5vw, 1.45rem)`, `-0.01em` tracking): step, point, and group headings inside a
  section.
- **Body** (regular, `1.0625rem`, `1.5` line height, `65–75ch` measure; long-form
  prose caps at `72ch`): usage and explanatory copy. Ledes run `1.125–1.2rem` in
  Secondary Ink at `40–60ch`.
- **Label** (medium, `0.9375rem`, `1.2` line height): compact native controls,
  navigation, and state labels.
- **Mono** (`0.875rem`, `1.6` line height): code blocks; inline code sits at `0.9em`
  of its surrounding text.

### Named Type Rules

**The Native Voice Rule.** Product UI uses the macOS system typeface. Custom display
lettering belongs to the Daisy wordmark, not application controls.

## 4. Layout

Content sits in one centered column, `1240px` at most, inside a fluid gutter of
`clamp(20px, 4vw, 48px)`. Spacing follows the six-step scale in the frontmatter,
from `0.5rem` to a fluid section step. Sections pad by the largest step and are
separated from each other by a 1px Rule, not a background change.

Multi-column arrangements collapse to one column at `900px`; at `640px` the header
drops its button and last navigation item. Docs pages pair a `220px` sticky
navigation column with a `72ch` prose column and stack at `900px`. Section heads cap
at `44rem`; headings balance and paragraphs wrap pretty.

On screens `1100px` and wider the landing page carries a left rail: each section's
content indents `92px` to clear one `60px` flower, and the closing section sits on
the rail, left-aligned, ending at the `72px` Download flower. Below `1100px` there is
no rail and the closing section is centered. The live desk runs only at `1024px` and
wider, with a fine pointer and motion allowed; otherwise a static desk stands in.

## 5. Elevation

The identity is predominantly flat. Depth belongs to a physical macOS app-icon tile
or a native system surface, not the U-link geometry.

- Brand fields and chain diagrams use no shadow.
- The app-icon tile may use a soft, neutral, offset shadow consistent with macOS.
- Menu-bar glyphs are flat monochrome shapes plus the connected yellow center.
- Web surfaces carry no shadow. They separate by 1px Rules, a 1px inset hairline on
  ghost buttons, and tonal steps from Daisy Paper to Paper Raised and Paper Pressed.

### Named Elevation Rules

**The Flat Chain Rule.** Links never gain depth, bevels, inner shadows, or glow.

## 6. Shapes

Actions and chips are full pills. Contained surfaces use gentle corners: `6px` for
navigation rows, `12px` for code blocks, `14px` for placeholders, `5px` for keycaps
and inline code. Step counters and list bullets are circles. Depicted displays are
graphite-bezel outlines with rounded corners, never device photography. The flower
and the U link are the only drawn identity geometry.

## 7. Components

### Primary identity mark

The large mark combines a centered flower with equal-size U links. On the flower's
left, the nearest link is `⊃`; on its right, the nearest link is `⊂`. The curved bowl
of each link is the end nearest the flower, while its two tips point away.

Additional links alternate and overlap slightly to interlock:
`⊂⊃⊂⊃ ✿ ⊂⊃⊂⊃`. Extend a chain by adding identical links, never by stretching a
link into a cable, capsule, or bar.

The chain path does not have to remain straight. It may bend at any link and flow
through horizontal, vertical, and diagonal runs like a physical chain. Turn the path
by rotating whole fixed-size links around their centers; never skew, taper, or
stretch them.

### Directional links

The bowl-toward-flower rule survives rotation:

- **Left:** `⊃`, curved bowl on the right beside the flower.
- **Right:** `⊂`, curved bowl on the left beside the flower.
- **Above:** `∪`, curved bottom beside the flower and tips pointing upward.
- **Below:** `∩`, curved top beside the flower and tips pointing downward.
- **Diagonal:** rotate the same geometry; the curved midpoint faces the flower and
  the open tips point away.

**The Flowing Chain Rule.** A chain may change direction from link to link. Every
turn is made with rotated, overlapping copies of the same U geometry, not a smooth
connector or a distorted link.

### Chain spacing

Units are flower units: the flower is about 60 across; a link has half-height 13,
tip length 30, and a 9.5 stroke with square ends.

**The Chain Spacing Rule.** A run between two flowers alternates `⊂⊃⊂⊃` with an even
link count, so the link nearest each flower has its bowl toward that flower. Facing
tips overlap about 6 units and back-to-back bowls about 5. When the distance is not
an exact fit, tighten every joint equally by up to 14 units so the run reaches from
flower to flower; past that, drop a pair. Keep about 44 units of clearance around
each flower. Links stay one flat color so overlapping facing tips close into ovals;
shading or a second color would expose the seam.

### Chain motion

A chain that moves behaves as a physical chain of rigid links, never a drawn stroke.

**The Rigid Link Rule.** Each segment of a moving chain is one closed link: a `⊂⊃`
pair at fixed spacing, drawn along the segment's own direction. Joints sit where
neighbouring bowls overlap, so the chain never opens at a bend, and no link ever
stretches, skews, or tapers in motion.

**The Taut Chain Rule.** A resting chain hangs nearly straight from flower to flower,
with about 0.4% slack under gravity 700 and damping 0.985. When displays move and the
slack passes about 1.5%, or the span outgrows the chain, the chain re-links to a new
link count instead of drooping or stretching.

**The Pull Rule.** When control crosses to another Mac, a pull passes along the chain
joint by joint, about 22ms apart, and the target flower blooms. The pull moves links;
it never lengthens them.

### Multi-Mac demonstration

Each display centers its own flower, regardless of screen size. Repeating links fill
the distance to each edge, may change direction along the way, and interlock across
display seams. Never put the flower on a bezel, at a seam, or only on a central
machine. Displays are drawn as graphite-bezel outlines on paper.

**The Magnet Spring Rule.** Arranged displays move on springs (stiffness 340, damping
ratio 0.7), snap magnetically to a neighbour within 56 units, and settle with a small
overshoot. The view eases to the new bounds and follows a display while it is
dragged. The animation loop sleeps once everything has settled.

### Landing thread

On wide screens a single chain runs down the landing page's left rail, flower to
flower, one `60px` flower per section, ending at the `72px` Download flower. Links
are placed with `chainAlong()`, which rotates whole interlocking links along the
path, so the Chain Spacing and Flowing Chain Rules hold on the rail.

**The Connect-on-Arrival Rule.** A thread flower starts disconnected, with a
Disconnected Gray center. When the chain reaches it, the center turns Connection
Yellow and the petals rotate 22.5°. The yellow still obeys the Yellow Signal Rule: it
means that section's flower is connected. Links scale in place from 0.55 to full size
as they scroll into view and retract on scroll back; they never stretch along the
path.

**The Complete Thread Rule.** With reduced motion the thread shows complete: every
link in place and every flower connected. Below `1100px` the rail and section flowers
are absent, and the Download flower shows connected.

### Menu-bar status

Both states use the identical flower silhouette with no side links or rings:

- **Connected:** gray petals and a yellow center.
- **Disconnected:** gray petals and a gray center.

Color is the only state difference. The glyph must remain recognizable at native
menu-bar size and must not acquire link “ears.”

### macOS app icon

The app icon is one centered Daisy bloom in front of two interlocking
anodized-gray chain links inside the macOS rounded-square tile. The links flow
diagonally behind the flower without stretching or crowding the tile. Use eight
softly organic graphite-gray petals, one crisp yellow center and generous optical
margin. The tile contains no wordmark, badge, glow, gradient, border or tiny
internal detail.

### Connection-state use beyond the menu bar

When a larger product surface genuinely needs to explain topology, it may reveal the
chain around a connected flower. A disconnected state removes the chain and turns
the center gray. Do not add topology merely as decoration.

### Native setup window

The setup window is an Operate surface: connection status and the next useful
action outrank brand decoration. A single large flower anchors the header with
its yellow brand center. Connection-state color changes belong to the menu-bar
glyph; the header mark does not turn gray when disconnected.

The current role choices are exactly **Host** and **Guest**. They are a compact,
secondary setup choice below connection direction, never a headline, device
identity, or sentence disguised as a menu item. One short helper line explains
that Host supplies the keyboard and trackpad while Guest receives input, and
that either role may wait or connect. Future positioned chains should reduce
the prominence of this choice rather than build more interface around it.

Conditional rows close up when hidden. Granted permissions retain a quiet text
status and remove their action buttons. Use native AppKit controls, system type,
keyboard focus, and platform spacing throughout.

Connection settings and permissions sit in separate adaptive rounded groups on
the native window background. Two-choice decisions use direct segmented controls;
menus are reserved for longer option sets. Coral marks the selected segments and
exactly one primary action. Before any peer is paired that action is **Pair a New
Peer**; after pairing it becomes **Start Sharing**. Yellow remains exclusive to the
connected flower state. A single short coral rule beneath the Connection heading
keeps the accent present while permission-gated actions are disabled.

### Buttons

- **Shape:** full pill.
- **Primary:** graphite fill, paper text, semibold `1rem`; hover darkens to
  `#1D2023`; active presses down 1px.
- **Ghost:** transparent with a 1px inset hairline; hover fills Paper Raised.
- **Small:** tighter padding at label size.
- **Desk controls:** white pills with a 1px border at label size; a pressed toggle
  inverts to graphite; disabled drops to 45% opacity.
- **Focus:** a 2px graphite outline offset 3px on every focusable element.
- **Native primary action:** the standard rounded AppKit button tinted Coral
  Annotation with white text. Only one action is primary at a time; native
  secondary buttons retain their system appearance.

### Chips

State chips are small semibold pills. **Planned** is graphite with paper text. A
**Draft** or **Placeholder** tag is a dashed-outline pill in Secondary Ink.

### Code blocks

White surface, 1px Rule border, `12px` corners, mono at `0.875rem/1.6`, horizontal
scroll rather than wrap, colored per the Monochrome Code Rule. Inline code sits on
Paper Raised with a 1px Rule border.

### Navigation

Header links are medium label-size Secondary Ink, turning graphite on hover; the
current page is graphite with a graphite underline. Docs navigation uses `6px`
rows: hover fills Paper Raised; the current page fills Paper Pressed in semibold
graphite.

### Placeholder art

**The Placeholder Rule.** Artwork awaiting the image pass ships as a placeholder
figure at its final aspect ratio, never an invented stand-in image. It has a dashed
`#B3B6B6` border, `14px` corners, and a faint 135° hairline hatch over Paper Raised,
centering a caption stack: a dashed **Placeholder** pill, the subject in semibold
graphite, the ratio and pixel size in tabular figures, and the intended alt text.
The figure is `role="img"`, labelled with that alt text. A missing wordmark uses the
same dashed, hatched treatment at its final size.

## 8. Do's Don'ts

### Do

- **Do** center one flower within every depicted display.
- **Do** keep every U link identical in size, weight, color, and corner character.
- **Do** aim every curved bowl toward its adjacent flower.
- **Do** create chain length through overlapping repetition.
- **Do** let chain paths bend and flow by rotating whole links at each turn.
- **Do** use the flower alone where icon scale cannot carry the chain cleanly.
- **Do** treat the approved comp as the north star while allowing production art to
  improve contours and optical spacing.
- **Do** set secondary text in Secondary Ink.
- **Do** mark missing artwork with the placeholder figure at its final aspect ratio.
- **Do** move chains as rigid closed links that re-link rather than stretch.
- **Do** light a thread flower only when the chain has reached it.
- **Do** show the thread and desk in their finished state when reduced motion is
  requested.

### Don't

- **Don't** reduce Daisy to two computers joined by one line.
- **Don't** use hub-and-spoke, wireless arcs, network graphs, or settings imagery as
  the identity.
- **Don't** stretch a U link into a cable, capsule, bar, or wavy connector.
- **Don't** force every chain into a straight line or distort a link to fake a turn.
- **Don't** point a link's open tips toward the flower.
- **Don't** make links black, glossy, shaded, gradient-filled, coral, or yellow.
- **Don't** add side-link ears to the menu-bar glyph or app icon.
- **Don't** use a yellow field; yellow is reserved for connected centers.
- **Don't** set text in coral or in anodized gray.
- **Don't** add hue to code highlighting.
- **Don't** open a chain at a bend or lengthen a link to absorb motion.
