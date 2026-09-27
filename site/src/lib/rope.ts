import { BOWL, linkAt, pitches, type Point } from "./marks";

// A chain between two flowers as a Verlet rope. Each segment is one closed
// link: a facing ⊂⊃ pair held rigid, so its tips can never part. Joints sit
// where neighbouring links' round bowls overlap, which stays closed at any
// bend angle. Links swing, sag and pull taut but never stretch.

interface Node {
  x: number;
  y: number;
  px: number;
  py: number;
}

export interface Rope {
  key: string;
  a: string;
  b: string;
  nodes: Node[];
  tip: number;
  bowl: number;
}

const CLEARANCE = 44;
const GRAVITY = 700;
const DAMPING = 0.985;
const ITERATIONS = 20;
const DT = 1 / 60;
const SLACK = 1.004;
const MAX_TIGHTEN = 14;

/** Joint positions at each end: half a bowl overlap in from the outermost bowls. */
export function anchors(ca: Point, cb: Point) {
  const dx = cb.x - ca.x;
  const dy = cb.y - ca.y;
  const d = Math.hypot(dx, dy) || 1;
  const reach = CLEARANCE + BOWL - pitches().bowl / 2;
  return {
    start: { x: ca.x + (dx / d) * reach, y: ca.y + (dy / d) * reach },
    end: { x: cb.x - (dx / d) * reach, y: cb.y - (dy / d) * reach },
    span: d - reach * 2,
  };
}

/** Link count and pitches for a rope spanning `span` joint to joint, with a little slack to sag. */
function sizeFor(span: number) {
  const loose = pitches();
  const seg = loose.tip + loose.bowl;
  const target = Math.max(span * SLACK, seg);
  let links = Math.max(1, Math.ceil(target / seg));
  while (links > 1 && (links * seg - target) / (links * 2) > MAX_TIGHTEN) links--;
  const tighten = Math.max(0, Math.min(MAX_TIGHTEN, (links * seg - target) / (links * 2)));
  return { links, tip: loose.tip - tighten, bowl: loose.bowl - tighten };
}

const segLength = (rope: Rope) => rope.tip + rope.bowl;

/** A rope laid from flower to flower, dropped by `sag` at its middle so it falls into place. */
export function makeRope(key: string, a: string, b: string, ca: Point, cb: Point, sag = 0): Rope {
  const { start, end, span } = anchors(ca, cb);
  const { links, tip, bowl } = sizeFor(span);
  const nodes: Node[] = [];
  for (let i = 0; i <= links; i++) {
    const t = i / links;
    const x = start.x + (end.x - start.x) * t;
    const y = start.y + (end.y - start.y) * t + Math.sin(Math.PI * t) * sag;
    nodes.push({ x, y, px: x, py: y });
  }
  return { key, a, b, nodes, tip, bowl };
}

/** Same path, new link count: lay the new links along the old rope's current shape. */
function relink(rope: Rope, span: number): Rope {
  const { links, tip, bowl } = sizeFor(span);
  const old = rope.nodes;
  const cum = [0];
  for (let i = 1; i < old.length; i++) cum.push(cum[i - 1] + Math.hypot(old[i].x - old[i - 1].x, old[i].y - old[i - 1].y));
  const oldLen = cum[cum.length - 1] || 1;
  const nodes: Node[] = [];
  for (let i = 0; i <= links; i++) {
    const want = (i / links) * oldLen;
    let j = 1;
    while (j < cum.length - 1 && cum[j] < want) j++;
    const f = Math.min(1, Math.max(0, (want - cum[j - 1]) / (cum[j] - cum[j - 1] || 1)));
    const p = old[j - 1];
    const q = old[j];
    const x = p.x + (q.x - p.x) * f;
    const y = p.y + (q.y - p.y) * f;
    const vx = p.x - p.px + (q.x - q.px - (p.x - p.px)) * f;
    const vy = p.y - p.py + (q.y - q.py - (p.y - p.py)) * f;
    nodes.push({ x, y, px: x - vx, py: y - vy });
  }
  return { ...rope, nodes, tip, bowl };
}

/** Advance one frame with the rope's ends held at its two flowers. */
export function stepRope(rope: Rope, ca: Point, cb: Point): Rope {
  const { start, end, span } = anchors(ca, cb);
  const length = segLength(rope) * (rope.nodes.length - 1);
  if (span > length * 0.999 || span < length * 0.985) rope = relink(rope, span);
  const rest = segLength(rope);
  const nodes = rope.nodes;
  const last = nodes.length - 1;
  for (let i = 1; i < last; i++) {
    const nd = nodes[i];
    const vx = (nd.x - nd.px) * DAMPING;
    const vy = (nd.y - nd.py) * DAMPING;
    nd.px = nd.x;
    nd.py = nd.y;
    nd.x += vx;
    nd.y += vy + GRAVITY * DT * DT;
  }
  for (let k = 0; k < ITERATIONS; k++) {
    nodes[0].x = start.x;
    nodes[0].y = start.y;
    nodes[last].x = end.x;
    nodes[last].y = end.y;
    for (let i = 0; i < last; i++) {
      const p = nodes[i];
      const q = nodes[i + 1];
      const dx = q.x - p.x;
      const dy = q.y - p.y;
      const d = Math.hypot(dx, dy) || 0.0001;
      const diff = (d - rest) / d;
      const pinP = i === 0;
      const pinQ = i + 1 === last;
      const wp = pinP ? 0 : pinQ ? 1 : 0.5;
      const wq = pinQ ? 0 : pinP ? 1 : 0.5;
      p.x += dx * diff * wp;
      p.y += dy * diff * wp;
      q.x -= dx * diff * wq;
      q.y -= dy * diff * wq;
    }
  }
  nodes[0].px = nodes[0].x = start.x;
  nodes[0].py = nodes[0].y = start.y;
  nodes[last].px = nodes[last].x = end.x;
  nodes[last].py = nodes[last].y = end.y;
  return rope;
}

/** Largest per-frame node movement, for deciding when the rope has settled. */
export function motion(rope: Rope): number {
  let m = 0;
  for (const nd of rope.nodes) m = Math.max(m, Math.abs(nd.x - nd.px) + Math.abs(nd.y - nd.py));
  return m;
}

/** Nudge each joint toward `from`'s far end, one after another, like a pull passing through the chain. */
export function tug(rope: Rope, from: string, strength = 6, stagger = 22): void {
  const n = rope.nodes.length;
  const forward = rope.a === from;
  const s = rope.nodes[0];
  const e = rope.nodes[n - 1];
  const d = Math.hypot(e.x - s.x, e.y - s.y) || 1;
  const ux = ((e.x - s.x) / d) * (forward ? 1 : -1);
  const uy = ((e.y - s.y) / d) * (forward ? 1 : -1);
  rope.nodes.forEach((nd, i) => {
    const order = forward ? i : n - 1 - i;
    window.setTimeout(() => {
      nd.px -= ux * strength;
      nd.py -= uy * strength - 2;
    }, order * stagger);
  });
}

/** Each segment is one closed link: ⊂ then ⊃ at fixed spacing along the segment's own direction. */
export function drawRope(rope: Rope): string {
  const nodes = rope.nodes;
  let out = "";
  for (let i = 0; i < nodes.length - 1; i++) {
    const p = nodes[i];
    const q = nodes[i + 1];
    const d = Math.hypot(q.x - p.x, q.y - p.y) || 1;
    const ux = (q.x - p.x) / d;
    const uy = (q.y - p.y) / d;
    const angle = (Math.atan2(uy, ux) * 180) / Math.PI;
    const first = rope.bowl / 2;
    const second = first + rope.tip;
    out += linkAt(p.x + ux * first, p.y + uy * first, angle, true);
    out += linkAt(p.x + ux * second, p.y + uy * second, angle, false);
  }
  return out;
}
