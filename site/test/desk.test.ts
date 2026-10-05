import { test } from 'node:test';
import assert from 'node:assert/strict';
import { adjacentPairs, initialState, interior, renderDesk, snapCandidate, step, type Screen } from '../src/lib/desk.ts';

function screen(id: string, x: number, y: number, w = 500, h = 300): Screen {
  return { id, x, y, w, h, kind: 'desktop', label: 'Display', planned: false };
}
function desk(screens: Screen[]) {
  return { ...initialState(), screens, home: screens[0].id, active: screens[0].id, pointer: { x: 0, y: 0 } };
}

test('four-display arrangement draws all five reachable paths in static and live topology', () => {
  const state = desk([
    screen('a', 0, 0, 470, 300), screen('b', 508, 0),
    screen('c', 0, 322, 620, 360), screen('d', 642, 348),
  ]);
  assert.deepEqual(adjacentPairs(state).map(p => p.key), ['a-b', 'a-c', 'b-c', 'b-d', 'c-d']);
  assert.equal((renderDesk(state).match(/data-chain=/g) ?? []).length, 5);
  state.active = 'b';
  const r = interior(state.screens[1]);
  state.pointer = { x: r.x + 200, y: r.y + r.h };
  assert.equal(step(state, 0, 1, false).kind, 'crossed');
  assert.equal(state.active, 'd');
});

test('diagonal pointer motion crosses a corner in both directions', () => {
  const state = desk([screen('a', 0, 0), screen('b', 522, 322)]);
  assert.equal(adjacentPairs(state).length, 1);
  state.pointer = { x: 487, y: 287 };
  assert.equal(step(state, 1, 1, false).kind, 'crossed');
  assert.equal(state.active, 'b');
  assert.equal(step(state, -1, -1, false).kind, 'crossed');
  assert.equal(state.active, 'a');
});

test('routing follows the ray rather than remapping the entire edge', () => {
  const state = desk([screen('a', 0, 0), screen('b', 522, 200)]);
  state.pointer = { x: 487, y: 50 };
  assert.equal(step(state, 1, 0, false).kind, 'moved');
  assert.equal(state.active, 'a');
  state.pointer.y = 250;
  assert.equal(step(state, 1, 0, false).kind, 'crossed');
  assert.equal(state.pointer.y, 250);
});

test('held buttons block crossing and distant displays have no chain', () => {
  const state = desk([screen('a', 0, 0), screen('b', 522, 0), screen('c', 1500, 0)]);
  state.pointer = { x: 487, y: 100 };
  assert.equal(step(state, 1, 0, true).kind, 'blocked');
  assert.equal(state.active, 'a');
  assert.deepEqual(adjacentPairs(state).map(p => p.key), ['a-b']);
});

test('arrangement can snap to a corner', () => {
  const state = desk([screen('a', 0, 0), screen('b', 522, 0)]);
  assert.deepEqual(snapCandidate(state, 'b', 522, 322), { x: 522, y: 322 });
});

test('the nearest display on the ray receives control', () => {
  const state = desk([screen('a', 0, 0), screen('b', 522, 0), screen('c', 1044, 0)]);
  state.pointer = { x: 487, y: 100 };
  assert.equal(step(state, 1000, 0, false).kind, 'crossed');
  assert.equal(state.active, 'b');
});
