import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { runInNewContext } from 'node:vm';
import { initialState, snap, snapCandidate } from '../src/lib/desk.ts';

test('reduced motion stops queued settling and makes subsequent placement immediate', () => {
  const source = readFileSync(new URL('../src/components/Desk.astro', import.meta.url), 'utf8');
  const handlers = ['wake', 'settleMotion'].map(name => {
    const start = source.indexOf(`  function ${name}(`);
    if (start < 0) return '';
    return source.slice(start, source.indexOf('\n  }', start) + 4);
  }).join('\n');
  const screen = { id: 'a', x: 100, y: 50 };
  const body = { x: 0, y: 0, vx: 10, vy: 10 };
  const calls: string[] = [];
  const context = {
    still: { matches: true }, state: { screens: [screen] }, bodies: new Map([['a', body]]),
    body: () => body, bounds: () => ({ x: 100, y: 50, w: 500, h: 300 }),
    adjacentPairs: () => [], paint() { calls.push('paint'); },
    svg: { getAnimations: () => [] },
    cancelAnimationFrame(id: number) { calls.push(`cancel:${id}`); },
    requestAnimationFrame() { calls.push('request'); return 2; }, tick() {},
  };
  const listenerStart = source.indexOf('  still.addEventListener("change"');
  const listener = source.slice(listenerStart, source.indexOf('\n  });', listenerStart) + 6);
  const script = `let running = true; let motionFrame = 1; let calm = 0; let drag = null;
    let ropes = new Map(); let view = null; let started = true; let changed;
    still.addEventListener = (_, callback) => { changed = callback; };
    ${handlers} ${listener}
    changed(); globalThis.runningAfter = running; globalThis.wakeAgain = wake;`;
  runInNewContext(stripTypeScriptTypes(script), context);
  assert.deepEqual(body, { x: 100, y: 50, vx: 0, vy: 0 });
  assert.deepEqual(calls, ['cancel:1', 'paint']);
  assert.equal((context as typeof context & { runningAfter: boolean }).runningAfter, false);
  screen.x = 200;
  (context as typeof context & { wakeAgain: () => void }).wakeAgain();
  assert.equal(body.x, 200);
  assert.deepEqual(calls, ['cancel:1', 'paint', 'cancel:0', 'paint']);
});

test('clicking a settling display preserves placement while dragging still moves it', () => {
  const source = readFileSync(new URL('../src/components/Desk.astro', import.meta.url), 'utf8');
  const handlers = ['onDown', 'onMove', 'onUp'].map(name => {
    const start = source.indexOf(`  function ${name}(`);
    const end = source.indexOf('\n  }', start) + 4;
    assert.ok(start >= 0 && end > start);
    return source.slice(start, end);
  }).join('\n');
  const state = initialState();
  state.arranging = true;
  const screen = state.screens[1];
  const drawn = { x: screen.x, y: screen.y };
  assert.ok(snap(state, screen.id, 0, 382, drawn));
  const placed = { x: screen.x, y: screen.y };
  let captures = 0;
  const context = {
    state, snap, snapCandidate, MAGNET: 56,
    byId: () => screen, body: () => drawn, toSvg: (e: { x: number; y: number }) => e,
    stage: { setPointerCapture: () => captures++, hasPointerCapture: () => captures > 0, releasePointerCapture: () => captures-- },
    root: { classList: { add() {}, remove() {} } },
    wake() {}, syncRopes() {}, render() {}, say() {}, name: () => screen.label,
  };
  const script = `let drag = null; let held = false; ${handlers}
    globalThis.down = onDown; globalThis.move = onMove; globalThis.up = onUp;`;
  runInNewContext(stripTypeScriptTypes(script), context);
  const api = context as typeof context & { down: Function; move: Function; up: Function };
  const event = { pointerId: 1, x: drawn.x, y: drawn.y, target: { closest: () => ({ dataset: { id: screen.id } }) } };
  api.down(event);
  api.up(event);
  assert.deepEqual({ x: screen.x, y: screen.y }, placed, 'a plain click must not commit the old drawn position');
  assert.equal(captures, 0);
  api.down(event);
  api.move({ ...event, x: drawn.x + 1 });
  api.up(event);
  assert.deepEqual({ x: screen.x, y: screen.y }, drawn, 'an actual drag still commits its snap target');
  assert.equal(captures, 0);
});
