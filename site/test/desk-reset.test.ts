import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { runInNewContext } from 'node:vm';
import { addScreen, initialState, interior, SPACES, type DeskState } from '../src/lib/desk.ts';

test('reset invalidates pending swipes and cancellation allows another swipe', () => {
  const source = readFileSync(new URL('../src/components/Desk.astro', import.meta.url), 'utf8');
  const handlers = ['swipe', 'action'].map(name => {
    const start = source.indexOf(`  function ${name}(`);
    assert.ok(start >= 0);
    return source.slice(start, source.indexOf('\n  }', start) + 4);
  }).join('\n');
  const state = initialState();
  state.active = addScreen(state)!.id;
  const pending: { resolve: () => void; reject: () => void }[] = [];
  let api: { swipe: (direction: number) => void; reset: () => void; snapshot: () => { state: DeskState; swiping: boolean } };
  const button = { setAttribute() {} };
  const context = {
    initialState, interior, SPACES, still: { matches: false }, state,
    byId: (id: string) => api.snapshot().state.screens.find(s => s.id === id),
    svg: { querySelector(selector: string) {
      if (!api.snapshot().state.screens.some(s => selector.includes(`"${s.id}"`))) return null;
      return { animate: () => ({ finished: { then(resolve: () => void, reject: () => void) { pending.push({ resolve, reject }); } } }) };
    } },
    root: { querySelectorAll: () => [], querySelector: () => button },
    render() {}, syncRopes() {}, say() {}, controlStatus: () => '', name: () => '',
  };
  runInNewContext(stripTypeScriptTypes(`let swiping = false; let swipeEpoch = 0;
    let offset; let ropes; const bodies = new Map(); ${handlers}
    globalThis.api = { swipe, reset: () => action('reset', null), snapshot: () => ({ state, swiping }) };`), context);
  api = (context as typeof context & { api: typeof api }).api;
  api.swipe(1);
  api.reset();
  assert.doesNotThrow(() => pending[0].resolve());
  assert.equal(api.snapshot().swiping, false);
  assert.deepEqual(Object.keys(api.snapshot().state.space), Object.keys(initialState().space));
  api.swipe(1);
  assert.equal(pending.length, 2, 'a new swipe can start after reset');
  pending[1].resolve();
  pending[2].resolve();
  assert.equal(api.snapshot().swiping, false);
  api.swipe(1);
  pending[3].reject();
  assert.equal(api.snapshot().swiping, false, 'a cancelled animation releases the swipe');
});
