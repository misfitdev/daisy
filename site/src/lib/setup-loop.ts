import { arrow } from "./desk";
import { chain, colors, flower } from "./marks";

// One drawing in a 480 x 320 field, posed as a pure function of time so the
// loop can pause anywhere and the static markup is just one more pose.

export const LOOP = 11.6;
/** Scene starts in seconds: Install, Allow, Pair. */
export const SCENES = [0, 3.4, 6.6];
/** The connected Pair pose, shown without motion. */
export const STILL = 11;

type Attrs = Record<string, string>;
type Move = [t0: number, t1: number, x: number, y: number];

const clamp01 = (k: number) => Math.min(1, Math.max(0, k));
const ease = (k: number) => 1 - Math.pow(1 - clamp01(k), 4);
const ramp = (t: number, a: number, b: number) => ease((t - a) / (b - a));
const f = (n: number) => n.toFixed(2);

function mix(a: string, b: string, k: number) {
  const pa = [1, 3, 5].map((i) => parseInt(a.slice(i, i + 2), 16));
  const pb = [1, 3, 5].map((i) => parseInt(b.slice(i, i + 2), 16));
  return `#${pa.map((v, i) => Math.round(v + (pb[i] - v) * k).toString(16).padStart(2, "0")).join("")}`;
}

function travel(t: number, start: [number, number], moves: Move[]) {
  let [x, y] = start;
  for (const [t0, t1, tx, ty] of moves) {
    if (t <= t0) break;
    const k = ramp(t, t0, t1);
    x += (tx - x) * k;
    y += (ty - y) * k;
  }
  return { x, y };
}

const TRACK_OFF = "#D9DADA";
const HAIRLINE = "#DEDDD7";
const ICON = { x: 140, y: 140, size: 56 };
const SLOT = { x: 341, y: 150, size: 34 };
const GRAB = { x: -10, y: -12 };
const SWITCH_Y = [102, 162];
const PAIR = [{ x: 130, y: 137 }, { x: 350, y: 137 }];
const CODE = "482913";
const FLOWER_SCALE = 0.5;

const links = chain(
  { x: PAIR[0].x / FLOWER_SCALE, y: PAIR[0].y / FLOWER_SCALE },
  { x: PAIR[1].x / FLOWER_SCALE, y: PAIR[1].y / FLOWER_SCALE },
);

export function stepAt(t: number): 0 | 1 | 2 {
  return t < SCENES[1] ? 0 : t < SCENES[2] ? 1 : 2;
}

/** Every changing attribute at time `t`, keyed by `data-k`. */
export function frame(t: number): Record<string, Attrs> {
  const out: Record<string, Attrs> = {};
  const desk = ramp(t, 0, 0.35) * (1 - ramp(t, 6.4, 6.6));
  const pair = ramp(t, 6.6, 6.95) * (1 - ramp(t, 11.4, 11.6));
  out.desk = { opacity: f(desk) };
  out.install = { opacity: f(ramp(t, 0, 0.35) * (1 - ramp(t, 3.2, 3.4))) };
  out.allow = { opacity: f(ramp(t, 3.4, 3.75) * (1 - ramp(t, 6.4, 6.6))) };
  out.pair = { opacity: f(pair) };

  let p: { x: number; y: number };
  let down = false;
  if (t < SCENES[1]) {
    p = travel(t, [330, 222], [[0.4, 1.1, 150, 152], [1.3, 2.2, 368, 179], [2.4, 2.9, 318, 226]]);
    down = t >= 1.15 && t < 2.25;
  } else if (t < SCENES[2]) {
    p = travel(t, [318, 226], [[3.6, 4.2, 322, 104], [4.65, 5.2, 322, 164], [5.6, 6.1, 392, 222]]);
    down = (t >= 4.3 && t < 4.42) || (t >= 5.3 && t < 5.42);
  } else {
    p = travel(t, [300, 244], [[7.0, 7.6, 372, 154], [9.2, 9.7, 410, 216]]);
    down = t >= 7.7 && t < 7.82;
  }
  out.pointer = { transform: `translate(${f(p.x)} ${f(p.y)}) scale(${down ? 0.9 : 1})`, opacity: f(t < SCENES[2] ? desk : pair) };

  // Install: the icon follows the pointer from grab to drop, then settles into its slot.
  const dragging = t >= 1.3 && t < 2.2;
  const held = t >= 2.2 ? { x: SLOT.x + SLOT.size / 2, y: SLOT.y + SLOT.size / 2 } : dragging ? { x: p.x + GRAB.x, y: p.y + GRAB.y } : ICON;
  const lift = 1 + 0.06 * ramp(t, 1.15, 1.3);
  const scale = t < 2.25 ? lift : lift + (SLOT.size / ICON.size - lift) * ramp(t, 2.25, 2.6);
  out.icon = { transform: `translate(${f(held.x)} ${f(held.y)}) scale(${f(scale)})` };
  out.slot = { opacity: f(1 - ramp(t, 2.25, 2.5)) };

  [4.3, 5.3].forEach((at, i) => {
    const k = ramp(t, at, at + 0.25);
    out[`track${i}`] = { fill: mix(TRACK_OFF, colors.graphite, k) };
    out[`knob${i}`] = { transform: `translate(${f(18 * k)} 0)` };
  });

  out.field = { stroke: mix(HAIRLINE, colors.graphite, ramp(t, 7.7, 7.85)) };
  for (let i = 0; i < CODE.length; i++) {
    const at = 7.9 + i * 0.2;
    out[`digit${i}`] = { opacity: f(ramp(t, at, at + 0.12)) };
  }
  const gone = ramp(t, 9.15, 9.35);
  out.panels = { opacity: f(1 - gone), transform: `translate(240 ${PAIR[0].y + 5}) scale(${f(1 - 0.04 * gone)}) translate(-240 ${-PAIR[0].y - 5})` };
  links.forEach((_, i) => {
    const k = ramp(t, 9.35 + i * 0.045, 9.75 + i * 0.045);
    out[`link${i}`] = { style: `opacity:${f(k)};transform:scale(${f(0.55 + 0.45 * k)})` };
  });
  const lit = mix(colors.gray, colors.yellow, ramp(t, 9.85, 10.3));
  const turn = 22.5 * ramp(t, 9.85, 10.45);
  for (let i = 0; i < 2; i++) {
    out[`center${i}`] = { fill: lit };
    out[`glyph${i}`] = { fill: lit };
    out[`petals${i}`] = { style: `transform:rotate(${f(turn)}deg)` };
  }
  return out;
}

const attrs = (a: Attrs | undefined) => (a ? Object.entries(a).map(([k, v]) => ` ${k}="${v}"`).join("") : "");

/** Tag the first match of `needle` in `markup` with a key and its attributes. */
function keyed(markup: string, needle: string, k: string, poses: Record<string, Attrs>) {
  return markup.replace(needle, `${needle} data-k="${k}"${attrs(poses[k])}`);
}

function screen(x: number, y: number, w: number, h: number, bezel: number, bar: number, glyph = "") {
  const ix = x + bezel;
  const iy = y + bezel;
  const iw = w - bezel * 2;
  const ih = h - bezel * 2;
  const cx = x + w / 2;
  const top = y + h;
  const neck = w * 0.21;
  const drop = h * 0.21;
  return `<path d="M ${cx - neck / 2} ${top} H ${cx + neck / 2} L ${cx + neck * 0.4} ${top + drop} H ${cx - neck * 0.4} Z" fill="#D9DADA"/><rect x="${cx - neck * 0.85}" y="${top + drop}" width="${neck * 1.7}" height="${bar * 0.6}" rx="${bar * 0.3}" fill="#C9CBCC"/><rect x="${x}" y="${y}" width="${w}" height="${h}" rx="${bezel + 3}" fill="${colors.graphite}"/><rect x="${ix}" y="${iy}" width="${iw}" height="${ih}" rx="3" fill="#FBFAF7"/><rect x="${ix}" y="${iy}" width="${iw}" height="${bar}" fill="#EFEEE9"/>${glyph}`;
}

function glyph(x: number, y: number, k?: string, poses: Record<string, Attrs> = {}) {
  const m = flower(colors.gray, "#8A9196", 0.17);
  return `<g transform="translate(${x} ${y})">${k ? keyed(m, 'class="flower-center"', k, poses) : m}</g>`;
}

function install(poses: Record<string, Attrs>) {
  const tiles = [0, 1, 2, 3, 4, 5]
    .map((i) => {
      const x = 245 + (i % 3) * 48;
      const y = 102 + Math.floor(i / 3) * 48;
      return i === 5
        ? `<rect data-k="slot"${attrs(poses.slot)} x="${x}" y="${y}" width="34" height="34" rx="8" fill="none" stroke="#B3B6B6" stroke-dasharray="4 3"/>`
        : `<rect x="${x}" y="${y}" width="34" height="34" rx="8" fill="#E6E5DF"/>`;
    })
    .join("");
  const half = ICON.size / 2;
  const icon = `<g data-k="icon"${attrs(poses.icon)}><rect x="${-half}" y="${-half}" width="${ICON.size}" height="${ICON.size}" rx="13" fill="#FFFFFF" stroke="${HAIRLINE}"/>${flower(colors.gray, colors.graphite, 0.62)}</g>`;
  return `<g data-k="install"${attrs(poses.install)}><rect x="230" y="60" width="165" height="150" rx="10" fill="#FFFFFF" stroke="${HAIRLINE}"/><text class="loop-label" x="244" y="81">Applications</text><path d="M 230 90.5 H 395" stroke="#ECEBE6"/>${tiles}${icon}</g>`;
}

function allow(poses: Record<string, Attrs>) {
  const rows = ["Accessibility", "Input Monitoring"]
    .map((label, i) => {
      const y = SWITCH_Y[i];
      return `<text class="loop-label" x="130" y="${y + 5}">${label}</text><rect data-k="track${i}"${attrs(poses[`track${i}`])} x="309" y="${y - 11}" width="40" height="22" rx="11"/><g data-k="knob${i}"${attrs(poses[`knob${i}`])}><circle cx="320" cy="${y}" r="9" fill="#FFFFFF"/></g>`;
    })
    .join("");
  return `<g data-k="allow"${attrs(poses.allow)}><rect x="110" y="72" width="260" height="120" rx="12" fill="#FFFFFF" stroke="${HAIRLINE}"/><path d="M 126 132.5 H 354" stroke="#ECEBE6"/>${rows}</g>`;
}

function pair(poses: Record<string, Attrs>) {
  const screens = PAIR.map((c, i) => screen(c.x - 100, c.y - 67, 200, 134, 9, 10, glyph(c.x + 84, c.y - 53, `glyph${i}`, poses))).join("");
  const chained = links.map((l, i) => keyed(l, "<path", `link${i}`, poses)).join("");
  const flowers = PAIR.map((c, i) => {
    let m = flower(colors.gray, colors.graphite, FLOWER_SCALE);
    m = keyed(m, 'class="petals"', `petals${i}`, poses);
    m = keyed(m, 'class="flower-center"', `center${i}`, poses);
    return `<g transform="translate(${c.x} ${c.y})">${m}</g>`;
  }).join("");
  const [a, b] = PAIR;
  const panel = (x: number, body: string, k = "") =>
    `<rect${k} x="${x - 75}" y="${a.y - 24}" width="150" height="58" rx="10" fill="#FFFFFF" stroke="${HAIRLINE}"/>${body}`;
  const code = `<text class="loop-code" x="${a.x}" y="${a.y + 13}" text-anchor="middle">${CODE.slice(0, 3)} ${CODE.slice(3)}</text>`;
  let slots = "";
  for (let i = 0; i < CODE.length; i++) {
    const x = b.x - 61 + i * 20 + (i >= 3 ? 10 : 0);
    slots += `<rect x="${x}" y="${a.y - 8}" width="16" height="26" rx="5" fill="#F4F4F2"/><text class="loop-digit" data-k="digit${i}"${attrs(poses[`digit${i}`])} x="${x + 8}" y="${a.y + 10}" text-anchor="middle">${CODE[i]}</text>`;
  }
  const field = panel(b.x, slots, ` data-k="field"${attrs(poses.field)}`);
  return `<g data-k="pair"${attrs(poses.pair)}>${screens}<g transform="scale(${FLOWER_SCALE})">${chained}</g>${flowers}<g data-k="panels"${attrs(poses.panels)}>${panel(a.x, code)}${field}</g></g>`;
}

/** The whole drawing posed at `t`. */
export function renderSetupLoop(t = STILL, opts: { pointer?: boolean } = {}): string {
  const poses = frame(t);
  if (opts.pointer === false) poses.pointer = { ...poses.pointer, opacity: "0" };
  const desk = `<g data-k="desk"${attrs(poses.desk)}>${screen(50, 12, 380, 240, 11, 12, glyph(403, 29))}${install(poses)}${allow(poses)}</g>`;
  const pointer = `<g class="loop-pointer" data-k="pointer"${attrs(poses.pointer)}>${arrow()}</g>`;
  return `<svg viewBox="0 0 480 320" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">${desk}${pair(poses)}${pointer}</svg>`;
}
