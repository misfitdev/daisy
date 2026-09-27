// Interim vector geometry for the Daisy flower and U link, drawn to the rules
// in DESIGN.md. Production artwork replaces these shapes without changing the
// rules: bowls face the nearest flower, links never stretch, links stay flat.

export const colors = {
  paper: "#F7F6F2",
  graphite: "#2E3235",
  link: "#737D84",
  yellow: "#FFD447",
  gray: "#A3A7AA",
};

const PETALS = 8;

/** Flower centered on the origin, about 60 units across at scale 1. */
export function flower(center: string, petal = colors.graphite, scale = 1): string {
  let petals = "";
  for (let i = 0; i < PETALS; i++) {
    petals += `<ellipse cx="0" cy="-17.5" rx="6.6" ry="11.2" transform="rotate(${i * (360 / PETALS)})"/>`;
  }
  return `<g class="flower" transform="scale(${scale})"><g class="petals" fill="${petal}">${petals}</g><circle class="flower-center" r="8.6" fill="${center}"/></g>`;
}

/** Link half-height, tip length and stroke, in flower units. */
export const LINK = { h: 13, tip: 30, stroke: 9.5 };

/**
 * U link with its bowl facing -x, centered on the origin. Rotate the whole
 * link to aim the bowl; never scale one axis.
 */
export function link(): string {
  const { h, tip, stroke } = LINK;
  const shift = -(tip - h) / 2;
  return `<path d="M ${tip + shift} ${-h} H ${shift} A ${h} ${h} 0 0 0 ${shift} ${h} H ${tip + shift}" fill="none" stroke="${colors.link}" stroke-width="${stroke}" stroke-linecap="butt"/>`;
}

export interface Point {
  x: number;
  y: number;
}

/** Link extent from its placement point: bowl side and tip side. */
export const BOWL = LINK.h + LINK.stroke / 2 + (LINK.tip - LINK.h) / 2;
export const TIPS = LINK.tip - (LINK.tip - LINK.h) / 2;

/** Spacing between link placement points: facing tips, then back-to-back bowls. */
export function pitches(tipLap = 6, bowlLap = 5) {
  return { tip: TIPS * 2 - tipLap, bowl: BOWL * 2 - bowlLap };
}

/** One link at a point; `towardStart` aims its bowl back along the path. */
export function linkAt(x: number, y: number, angle: number, towardStart: boolean, attrs = ""): string {
  const rot = towardStart ? angle : angle + 180;
  return `<g class="link"${attrs} transform="translate(${x.toFixed(2)} ${y.toFixed(2)}) rotate(${rot.toFixed(2)})">${link()}</g>`;
}

/**
 * Interlocking links along a polyline between two flowers. Bends are made by
 * rotating whole links to the path's direction, never by distorting one.
 */
export function chainAlong(points: Point[], clearance = 44, tipLap = 6, bowlLap = 5): string[] {
  const segs: { a: Point; b: Point; len: number; at: number }[] = [];
  let total = 0;
  for (let i = 1; i < points.length; i++) {
    const a = points[i - 1];
    const b = points[i];
    const len = Math.hypot(b.x - a.x, b.y - a.y);
    segs.push({ a, b, len, at: total });
    total += len;
  }
  const loose = pitches(tipLap, bowlLap);
  const room = total - (clearance + BOWL) * 2;
  if (room < loose.tip) return [];
  const MAX_TIGHTEN = 14;
  let pairs = Math.max(1, Math.ceil((room + loose.bowl) / (loose.tip + loose.bowl)));
  const spanAt = (p: number) => p * loose.tip + (p - 1) * loose.bowl;
  while (pairs > 1 && (spanAt(pairs) - room) / (pairs * 2 - 1) > MAX_TIGHTEN) pairs--;
  const tighten = Math.max(0, (spanAt(pairs) - room) / (pairs * 2 - 1));
  const tipPitch = loose.tip - tighten;
  const bowlPitch = loose.bowl - tighten;
  const span = pairs * tipPitch + (pairs - 1) * bowlPitch;
  const out: string[] = [];
  let s = (total - span) / 2;
  for (let i = 0; i < pairs * 2; i++) {
    const seg = segs.find((g) => s <= g.at + g.len) ?? segs[segs.length - 1];
    const f = (s - seg.at) / seg.len;
    const x = seg.a.x + (seg.b.x - seg.a.x) * f;
    const y = seg.a.y + (seg.b.y - seg.a.y) * f;
    const angle = (Math.atan2(seg.b.y - seg.a.y, seg.b.x - seg.a.x) * 180) / Math.PI;
    out.push(linkAt(x, y, angle, i % 2 === 0, ` style="--i:${i}"`));
    s += i % 2 === 0 ? tipPitch : bowlPitch;
  }
  return out;
}

/**
 * Interlocking links between two flower centers, alternating ⊂⊃⊂⊃. An even
 * count puts the bowl of the link nearest each flower toward that flower.
 * Facing tips overlap by `tipLap` and back-to-back bowls by `bowlLap`, so each
 * U still reads as its own link.
 */
export function chain(a: Point, b: Point, clearance = 44, tipLap = 6, bowlLap = 5): string[] {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const d = Math.hypot(dx, dy);
  const loose = { tip: TIPS * 2 - tipLap, bowl: BOWL * 2 - bowlLap };
  const room = d - (clearance + BOWL) * 2;
  if (room < loose.tip) return [];
  // Enough pairs to reach both flowers, then tighten the overlaps to fit.
  const MAX_TIGHTEN = 14;
  let pairs = Math.max(1, Math.ceil((room + loose.bowl) / (loose.tip + loose.bowl)));
  const spanAt = (p: number) => p * loose.tip + (p - 1) * loose.bowl;
  while (pairs > 1 && (spanAt(pairs) - room) / (pairs * 2 - 1) > MAX_TIGHTEN) pairs--;
  const tighten = Math.max(0, (spanAt(pairs) - room) / (pairs * 2 - 1));
  const tipPitch = loose.tip - tighten;
  const bowlPitch = loose.bowl - tighten;
  const span = pairs * tipPitch + (pairs - 1) * bowlPitch;
  const angle = (Math.atan2(dy, dx) * 180) / Math.PI;
  const links: string[] = [];
  let t = (d - span) / 2;
  for (let i = 0; i < pairs * 2; i++) {
    const towardA = i % 2 === 0;
    const x = a.x + (dx * t) / d;
    const y = a.y + (dy * t) / d;
    const rot = towardA ? angle : angle + 180;
    links.push(`<g class="link" data-t="${(t / d).toFixed(3)}" data-toward-a="${towardA ? 1 : 0}" transform="translate(${x.toFixed(2)} ${y.toFixed(2)}) rotate(${rot.toFixed(2)})">${link()}</g>`);
    t += towardA ? tipPitch : bowlPitch;
  }
  return links;
}
