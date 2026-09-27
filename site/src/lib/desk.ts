import { chain, colors, flower, type Point } from "./marks";

export type Side = "left" | "right" | "above" | "below";
export type Kind = "desktop" | "laptop";

export interface Screen {
  id: string;
  kind: Kind;
  x: number;
  y: number;
  w: number;
  h: number;
  label: string;
  planned: boolean;
}

export interface DeskState {
  screens: Screen[];
  home: string;
  active: string;
  pointer: Point;
  connected: boolean;
  arranging: boolean;
  space: Record<string, number>;
  notes: Record<string, string>;
}

export const GAP = 22;
export const SPACES = 3;
export const MAX_SCREENS = 4;
const MIN_OVERLAP = 90;
const BEZEL: Record<Kind, number> = { desktop: 13, laptop: 11 };

export function initialState(): DeskState {
  const home: Screen = { id: "a", kind: "desktop", x: 0, y: 0, w: 620, h: 360, label: "This Mac", planned: false };
  const laptop: Screen = { id: "b", kind: "laptop", x: 620 + GAP, y: 40, w: 470, h: 300, label: "Laptop", planned: false };
  const inner = interior(home);
  return {
    screens: [home, laptop],
    home: home.id,
    active: home.id,
    pointer: { x: inner.x + inner.w * 0.62, y: inner.y + inner.h * 0.58 },
    connected: true,
    arranging: false,
    space: {},
    notes: {},
  };
}

export function interior(s: Screen) {
  const b = BEZEL[s.kind];
  return { x: s.x + b, y: s.y + b, w: s.w - b * 2, h: s.h - b * 2 };
}

export function center(s: Screen): Point {
  const r = interior(s);
  return { x: r.x + r.w / 2, y: r.y + r.h / 2 };
}

function overlap(a0: number, a1: number, b0: number, b1: number) {
  return Math.min(a1, b1) - Math.max(a0, b0);
}

/** The side of `a` that `b` sits on, when the two share an edge. */
export function adjacentSide(a: Screen, b: Screen): Side | null {
  const tol = 1.5;
  const oy = overlap(a.y, a.y + a.h, b.y, b.y + b.h);
  const ox = overlap(a.x, a.x + a.w, b.x, b.x + b.w);
  if (oy > 0 && Math.abs(b.x - (a.x + a.w) - GAP) < tol) return "right";
  if (oy > 0 && Math.abs(a.x - (b.x + b.w) - GAP) < tol) return "left";
  if (ox > 0 && Math.abs(b.y - (a.y + a.h) - GAP) < tol) return "below";
  if (ox > 0 && Math.abs(a.y - (b.y + b.h) - GAP) < tol) return "above";
  return null;
}

export function neighbors(state: DeskState, s: Screen, side: Side): Screen[] {
  return state.screens.filter((o) => o !== s && adjacentSide(s, o) === side);
}

function byId(state: DeskState, id: string): Screen {
  const s = state.screens.find((x) => x.id === id);
  if (!s) throw new Error(`no screen ${id}`);
  return s;
}

export type Step =
  | { kind: "moved" }
  | { kind: "blocked"; side: Side }
  | { kind: "crossed"; from: string; to: string };

/**
 * Move the pointer by a delta inside the active screen. Pushing through an
 * edge with a Mac beyond it moves control there; a held button never crosses.
 * One neighbor on an edge maps proportionally, as the product does today.
 */
export function step(state: DeskState, dx: number, dy: number, held: boolean): Step {
  const s = byId(state, state.active);
  const r = interior(s);
  const p = state.pointer;
  const nx = p.x + dx;
  const ny = p.y + dy;
  let side: Side | null = null;
  if (nx > r.x + r.w) side = "right";
  else if (nx < r.x) side = "left";
  else if (ny > r.y + r.h) side = "below";
  else if (ny < r.y) side = "above";

  const clamp = () => {
    p.x = Math.min(r.x + r.w, Math.max(r.x, nx));
    p.y = Math.min(r.y + r.h, Math.max(r.y, ny));
  };

  if (!side || !state.connected) {
    clamp();
    return { kind: "moved" };
  }
  const beyond = neighbors(state, s, side);
  if (beyond.length === 0) {
    clamp();
    return { kind: "moved" };
  }
  if (held) {
    clamp();
    return { kind: "blocked", side };
  }

  const horizontal = side === "left" || side === "right";
  let target: Screen;
  let along: number;
  if (beyond.length === 1) {
    target = beyond[0];
    const frac = horizontal ? (p.y - r.y) / r.h : (p.x - r.x) / r.w;
    const t = interior(target);
    along = horizontal ? t.y + frac * t.h : t.x + frac * t.w;
  } else {
    const at = horizontal ? p.y : p.x;
    const span = (o: Screen) => {
      const t = interior(o);
      return horizontal ? [t.y, t.y + t.h] : [t.x, t.x + t.w];
    };
    target =
      beyond.find((o) => {
        const [a, b] = span(o);
        return at >= a && at <= b;
      }) ??
      beyond.reduce((best, o) => {
        const dist = (x: Screen) => {
          const [a, b] = span(x);
          return Math.min(Math.abs(at - a), Math.abs(at - b));
        };
        return dist(o) < dist(best) ? o : best;
      });
    const [a, b] = span(target);
    along = Math.min(b, Math.max(a, at));
  }

  const t = interior(target);
  const inset = 2;
  if (side === "right") state.pointer = { x: t.x + inset, y: along };
  if (side === "left") state.pointer = { x: t.x + t.w - inset, y: along };
  if (side === "below") state.pointer = { x: along, y: t.y + inset };
  if (side === "above") state.pointer = { x: along, y: t.y + t.h - inset };
  const from = state.active;
  state.active = target.id;
  return { kind: "crossed", from, to: target.id };
}

/** Control-Option-Command-Escape: control returns to the Mac with the keyboard. */
export function reclaim(state: DeskState): string | null {
  if (state.active === state.home) return null;
  const from = state.active;
  const home = byId(state, state.home);
  const r = interior(home);
  state.active = home.id;
  state.pointer = { x: r.x + r.w / 2, y: r.y + r.h / 2 };
  return from;
}

function intersects(a: Screen, b: Screen) {
  return overlap(a.x, a.x + a.w, b.x - GAP + 1, b.x + b.w + GAP - 1) > 0 && overlap(a.y, a.y + a.h, b.y - GAP + 1, b.y + b.h + GAP - 1) > 0;
}

function connectedGraph(screens: Screen[]) {
  const seen = new Set([screens[0].id]);
  const queue = [screens[0]];
  while (queue.length) {
    const s = queue.shift()!;
    for (const o of screens) {
      if (!seen.has(o.id) && adjacentSide(s, o)) {
        seen.add(o.id);
        queue.push(o);
      }
    }
  }
  return seen.size === screens.length;
}

function clampNum(v: number, lo: number, hi: number) {
  return Math.min(hi, Math.max(lo, v));
}

/** Candidate positions for `s` flush against every edge of every other screen. */
function slots(state: DeskState, s: Screen, near: Point) {
  const out: { x: number; y: number }[] = [];
  for (const o of state.screens) {
    if (o.id === s.id) continue;
    const sy = clampNum(near.y, o.y - s.h + MIN_OVERLAP, o.y + o.h - MIN_OVERLAP);
    const sx = clampNum(near.x, o.x - s.w + MIN_OVERLAP, o.x + o.w - MIN_OVERLAP);
    out.push({ x: o.x + o.w + GAP, y: sy });
    out.push({ x: o.x - GAP - s.w, y: sy });
    out.push({ x: sx, y: o.y + o.h + GAP });
    out.push({ x: sx, y: o.y - GAP - s.h });
  }
  return out;
}

/** The nearest free edge slot for screen `id` near (x, y), or null when none is valid. */
export function snapCandidate(state: DeskState, id: string, x: number, y: number): Point | null {
  const s = byId(state, id);
  const candidates = slots(state, s, { x, y })
    .map((c) => ({ ...c, d: Math.hypot(c.x - x, c.y - y) }))
    .sort((a, b) => a.d - b.d);
  for (const c of candidates) {
    const moved = { ...s, x: c.x, y: c.y };
    const others = state.screens.filter((o) => o.id !== id);
    if (others.some((o) => intersects(moved, o))) continue;
    const next = state.screens.map((o) => (o.id === id ? moved : o));
    if (!connectedGraph(next)) continue;
    return { x: c.x, y: c.y };
  }
  return null;
}

/** Snap a dragged screen to the nearest free edge; false leaves it where it was. */
export function snap(state: DeskState, id: string, x: number, y: number, origin: Point): boolean {
  const s = byId(state, id);
  const c = snapCandidate(state, id, x, y);
  if (c) {
    const dx = c.x - s.x;
    const dy = c.y - s.y;
    s.x = c.x;
    s.y = c.y;
    if (state.active === id) state.pointer = { x: state.pointer.x + dx, y: state.pointer.y + dy };
    return true;
  }
  const back = { x: origin.x - s.x, y: origin.y - s.y };
  if (state.active === id) state.pointer = { x: state.pointer.x + back.x, y: state.pointer.y + back.y };
  s.x = origin.x;
  s.y = origin.y;
  return false;
}

export function addScreen(state: DeskState): Screen | null {
  if (state.screens.length >= MAX_SCREENS) return null;
  const id = String.fromCharCode(97 + state.screens.length);
  const s: Screen = { id, kind: "desktop", x: 0, y: 0, w: 500, h: 300, label: "Another Mac · planned", planned: true };
  const last = state.screens[state.screens.length - 1];
  const order = [last, ...state.screens.filter((o) => o !== last)];
  for (const o of order) {
    const cy = o.y + (o.h - s.h) / 2;
    const cx = o.x + (o.w - s.w) / 2;
    const tries = [
      { x: o.x + o.w + GAP, y: cy },
      { x: cx, y: o.y + o.h + GAP },
      { x: o.x - GAP - s.w, y: cy },
      { x: cx, y: o.y - GAP - s.h },
    ];
    for (const t of tries) {
      const moved = { ...s, ...t };
      if (!state.screens.some((x) => intersects(moved, x))) {
        state.screens.push(moved);
        return moved;
      }
    }
  }
  return null;
}

/** Where the other Macs sit relative to the Mac with the keyboard. */
export function homeSides(state: DeskState): Side[] {
  const home = byId(state, state.home);
  return state.screens.flatMap((o) => {
    const side = o === home ? null : adjacentSide(home, o);
    return side ? [side] : [];
  });
}

function hasBelow(state: DeskState, s: Screen) {
  return state.screens.some((o) => o !== s && adjacentSide(s, o) === "below");
}

export function bounds(state: DeskState) {
  const home = byId(state, state.home);
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const s of state.screens) {
    x0 = Math.min(x0, s.x - (s.kind === "laptop" ? 10 : 0));
    x1 = Math.max(x1, s.x + s.w + (s.kind === "laptop" ? 10 : 0));
    y0 = Math.min(y0, s.y);
    y1 = Math.max(y1, s.y + s.h + (hasBelow(state, s) ? 0 : s.kind === "desktop" ? 72 : 18));
  }
  const kb = keyboardBox(home, state);
  x0 = Math.min(x0, kb.x);
  x1 = Math.max(x1, kb.x + kb.w);
  y1 = Math.max(y1, kb.y + kb.h);
  const pad = 36;
  return { x: x0 - pad, y: y0 - pad, w: x1 - x0 + pad * 2, h: y1 - y0 + pad * 2 };
}

function keyboardBox(home: Screen, state: DeskState) {
  const lowest = Math.max(...state.screens.map((s) => s.y + s.h));
  const y = Math.max(home.y + home.h + 104, lowest + 40);
  const w = 430;
  return { x: home.x + home.w / 2 - w / 2, y, w, h: 44 };
}

function stand(s: Screen) {
  if (s.kind === "laptop") {
    const w = s.w + 20;
    const x = s.x - 10;
    const y = s.y + s.h;
    return `<path d="M ${x} ${y} H ${x + w} L ${x + w - 8} ${y + 14} H ${x + 8} Z" fill="#D9DADA"/><rect x="${s.x + s.w / 2 - 34}" y="${y}" width="68" height="5" rx="2.5" fill="#BFC2C3"/>`;
  }
  const cx = s.x + s.w / 2;
  const y = s.y + s.h;
  return `<path d="M ${cx - 46} ${y} H ${cx + 46} L ${cx + 36} ${y + 62} H ${cx - 36} Z" fill="#D9DADA"/><rect x="${cx - 78}" y="${y + 62}" width="156" height="8" rx="4" fill="#C9CBCC"/>`;
}

function windowShape(x: number, y: number, w: number, h: number, body = "") {
  return `<g class="window"><rect x="${x}" y="${y}" width="${w}" height="${h}" rx="7" fill="#FFFFFF" stroke="#DEDDD7"/><path d="M ${x} ${y + 17} H ${x + w}" stroke="#ECEBE6"/><g fill="#CFD1D2"><circle cx="${x + 11}" cy="${y + 9}" r="3"/><circle cx="${x + 21}" cy="${y + 9}" r="3"/><circle cx="${x + 31}" cy="${y + 9}" r="3"/></g>${body}</g>`;
}

function escapeXml(s: string) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function spaceLayer(state: DeskState, s: Screen, space: number) {
  const r = interior(s);
  const note = state.notes[`${s.id}:${space}`] ?? "";
  const active = state.active === s.id;
  const nx = r.x + 16;
  const ny = r.y + 28;
  const nw = Math.min(210, r.w * 0.4);
  const text = note
    ? `<text class="note-text" x="${nx + 12}" y="${ny + 38}">${escapeXml(note.slice(-26))}</text>`
    : active
      ? `<text class="note-hint" x="${nx + 12}" y="${ny + 38}">Type something</text>`
      : "";
  const caret = active && state.connected ? `<rect class="caret" x="${nx + 12 + Math.min(note.length, 26) * 6.1}" y="${ny + 28}" width="1.6" height="13" fill="${colors.graphite}"/>` : "";
  const extras = [
    () => windowShape(r.x + r.w - Math.min(190, r.w * 0.36) - 16, r.y + r.h - 108, Math.min(190, r.w * 0.36), 92),
    () => windowShape(r.x + r.w - 150, r.y + 30, 134, 84) + windowShape(r.x + 26, r.y + r.h - 96, 170, 80),
    () => "",
  ];
  return `<g class="space" data-screen="${s.id}">${windowShape(nx, ny, nw, 58, text + caret)}${extras[space % SPACES]()}</g>`;
}

function menuBar(state: DeskState, s: Screen) {
  const r = interior(s);
  const space = state.space[s.id] ?? 0;
  let dots = "";
  for (let i = 0; i < SPACES; i++) {
    dots += `<circle cx="${r.x + r.w - 58 + i * 9}" cy="${r.y + 7}" r="2.4" fill="${i === space ? colors.graphite : "#C8CACB"}"/>`;
  }
  const glyph = `<g transform="translate(${r.x + r.w - 16} ${r.y + 7})">${flower(state.connected ? colors.yellow : colors.gray, "#8A9196", 0.19)}</g>`;
  return `<rect x="${r.x}" y="${r.y}" width="${r.w}" height="14" fill="#EFEEE9"/>${dots}${glyph}`;
}

/** Pairs of screens that share an edge, as `[a, b]` with a stable key. */
export function adjacentPairs(state: DeskState): { key: string; a: Screen; b: Screen }[] {
  const out: { key: string; a: Screen; b: Screen }[] = [];
  state.screens.forEach((s, i) => {
    for (const o of state.screens.slice(i + 1)) {
      if (adjacentSide(s, o)) out.push({ key: `${s.id}-${o.id}`, a: s, b: o });
    }
  });
  return out;
}

export function renderDesk(state: DeskState, opts: { pointer?: boolean; liveChains?: boolean } = {}): string {
  const b = bounds(state);
  const home = byId(state, state.home);
  const parts: string[] = [];
  parts.push(`<defs>${state.screens.map((s) => {
    const r = interior(s);
    return `<clipPath id="clip-${s.id}"><rect x="${r.x}" y="${r.y}" width="${r.w}" height="${r.h}" rx="3"/></clipPath>`;
  }).join("")}</defs>`);

  const kb = keyboardBox(home, state);
  let keys = "";
  for (let row = 0; row < 4; row++) {
    for (let col = 0; col < 14; col++) {
      keys += `<rect x="${kb.x + 7 + col * 18.3}" y="${kb.y + 6 + row * 8.3}" width="15.5" height="6" rx="1.5" fill="#F4F4F2"/>`;
    }
  }
  parts.push(`<g class="input-devices" data-follow="${home.id}" aria-hidden="true"><rect x="${kb.x}" y="${kb.y}" width="270" height="${kb.h}" rx="7" fill="#DCDDDC"/>${keys}<rect x="${kb.x + 290}" y="${kb.y - 4}" width="140" height="${kb.h + 8}" rx="9" fill="#E8E9E8" stroke="#D3D5D5"/></g>`);

  for (const s of state.screens) {
    const r = interior(s);
    const space = state.space[s.id] ?? 0;
    const bottom = hasBelow(state, s) ? "" : stand(s);
    const planned = s.planned
      ? `<g class="planned-tag"><rect x="${r.x + r.w - 92}" y="${r.y + 24}" width="78" height="22" rx="11" fill="${colors.graphite}"/><text x="${r.x + r.w - 53}" y="${r.y + 39}" text-anchor="middle" fill="${colors.paper}">Planned</text></g>`
      : "";
    const labelY = hasBelow(state, s) ? s.y + s.h + 15 : s.y + s.h + (s.kind === "desktop" ? 92 : 38);
    const label = s.id === state.home ? `${s.label} · keyboard and trackpad` : s.label;
    parts.push(
      `<g class="screen${s.planned ? " is-planned" : ""}${state.active === s.id ? " is-active" : ""}" data-id="${s.id}">${bottom}<rect class="bezel" x="${s.x}" y="${s.y}" width="${s.w}" height="${s.h}" rx="${s.kind === "laptop" ? 14 : 16}" fill="${colors.graphite}"/><g clip-path="url(#clip-${s.id})"><rect x="${r.x}" y="${r.y}" width="${r.w}" height="${r.h}" fill="#FBFAF7"/><g class="space-wrap">${spaceLayer(state, s, space)}</g>${menuBar(state, s)}${planned}</g><text class="screen-label" x="${s.x + s.w / 2}" y="${labelY}" text-anchor="middle">${escapeXml(label)}</text></g>`,
    );
  }

  if (opts.liveChains) {
    parts.push(`<g class="chains-live"></g>`);
  } else if (state.connected) {
    const seen = new Set<string>();
    for (const s of state.screens) {
      for (const o of state.screens) {
        const key = [s.id, o.id].sort().join("-");
        if (s === o || seen.has(key) || !adjacentSide(s, o)) continue;
        seen.add(key);
        parts.push(`<g class="chain" data-chain="${key}" data-from="${s.id}">${chain(center(s), center(o)).join("")}</g>`);
      }
    }
  }

  for (const s of state.screens) {
    const c = center(s);
    parts.push(`<g class="flower-at" data-id="${s.id}" transform="translate(${c.x} ${c.y})">${flower(state.connected ? colors.yellow : colors.gray, colors.graphite, 1.22)}</g>`);
  }

  if (opts.pointer !== false && !state.arranging) {
    parts.push(`<g class="pointer" transform="translate(${state.pointer.x} ${state.pointer.y})">${arrow()}</g>`);
  }
  parts.push(`<g class="notice" aria-hidden="true"></g>`);

  return `<svg class="desk-svg" viewBox="${b.x} ${b.y} ${b.w} ${b.h}" preserveAspectRatio="xMidYMid meet" xmlns="http://www.w3.org/2000/svg">${parts.join("")}</svg>`;
}

/** macOS-style arrow, hotspot at the origin. */
export function arrow(): string {
  return `<path d="M0 0 L0 23 L5.6 17.6 L9.4 26.4 L13.4 24.7 L9.7 16.1 L17.2 16.1 Z" fill="#111" stroke="#fff" stroke-width="1.6" stroke-linejoin="round"/>`;
}
