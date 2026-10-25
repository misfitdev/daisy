import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { runInNewContext } from 'node:vm';

test('reduced-motion notices expire without fading and repeated notices renew the timeout', () => {
  const source = readFileSync(new URL('../src/components/Desk.astro', import.meta.url), 'utf8');
  const start = source.indexOf('  function notice(');
  assert.ok(start >= 0);
  const handler = source.slice(start, source.indexOf('\n  }', start) + 4);
  for (const reduced of [true, false]) {
    let animated = 0;
    let cancelled = 0;
    let nextTimer = 0;
    const timers = new Map<number, () => void>();
    const group = {
      innerHTML: '',
      getAnimations: () => [{ cancel() { cancelled++; } }],
      animate() { animated++; },
      replaceChildren() { this.innerHTML = ''; },
    };
    const context = {
      state: { active: 'a', pointer: { x: 100, y: 100 } },
      still: { matches: reduced }, byId: () => ({}), interior: () => ({ x: 0, y: 0, w: 500, h: 300 }),
      svg: { querySelector: () => group }, say() {},
      window: {
        clearTimeout(id: number) { timers.delete(id); },
        setTimeout(callback: () => void, delay: number) {
          assert.equal(delay, 1400);
          timers.set(++nextTimer, callback);
          return nextTimer;
        },
      },
    };
    runInNewContext(stripTypeScriptTypes(`let noticeTimer = 0; ${handler}; globalThis.show = notice;`), context);
    const show = (context as typeof context & { show: (side: string) => void }).show;
    show('right');
    assert.ok(group.innerHTML.includes('Release the button to cross'));
    assert.equal(animated, reduced ? 0 : 1);
    show('below');
    assert.equal(cancelled, 2);
    assert.equal(timers.size, reduced ? 1 : 0);
    if (reduced) {
      assert.ok(!timers.has(1), 'the old timeout cannot remove a new notice');
      timers.get(2)!();
      assert.equal(group.innerHTML, '');
    } else {
      assert.equal(animated, 2);
    }
  }
});
