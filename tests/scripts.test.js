// Scripts inserted after the page has loaded.

describe('inserted scripts', () => {
  test('inline scripts run on insertion, once', () => {
    page.setContent('<div id=host></div>');
    const r = page.evaluate(`(() => {
      window.count = 0;
      const s = document.createElement('script');
      s.textContent = 'window.count++; window.cs = document.currentScript === s0';
      window.s0 = s;
      document.getElementById('host').appendChild(s);
      document.body.appendChild(s); // moving it does not run it again
      return window.count + ' ' + window.cs;
    })()`);
    expect(r).toBe('1 true');
  });

  test('scripts parsed by innerHTML never run, even when moved', () => {
    page.setContent('<div id=host></div>');
    page.evaluate(`document.getElementById('host').innerHTML = '<script>window.viaInner = 1<\\/script>'; window.viaInner = window.viaInner || 0`);
    expect(page.evaluate('viaInner')).toBe(0);
    page.evaluate(`const d = document.createElement('div'); d.innerHTML = '<script>window.moved = 1<\\/script>'; document.body.appendChild(d); window.moved = window.moved || 0`);
    expect(page.evaluate('moved')).toBe(0);
  });

  test('external scripts load asynchronously and fire load', () => {
    page.goto('fixtures/scripts/../modules/index.html');
    page.evaluate(`
      window.events = [];
      for (const name of ['a', 'b']) {
        const s = document.createElement('script');
        s.id = 'script-' + name;
        s.src = '../scripts/' + name + '.js';
        s.onload = () => events.push(name + ' load');
        document.head.appendChild(s);
      }
      const bad = document.createElement('script');
      bad.src = '../scripts/missing.js';
      bad.onerror = () => events.push('missing error');
      document.head.appendChild(bad);
    `);
    page.waitForFunction('events.length === 3', 3000);
    expect(page.evaluate('events.slice().sort().join()')).toBe('a load,b load,missing error');
    expect(page.evaluate('order.slice().sort().join()')).toBe('a,b');
    expect(page.evaluate('fromA')).toBe('script-a');
  });

  test('module scripts can be inserted too', () => {
    page.goto('fixtures/modules/index.html');
    page.evaluate(`const s = document.createElement('script'); s.type = 'module'; s.src = '../scripts/mod.js'; document.head.appendChild(s);`);
    page.waitForFunction('window.moduleRan === true', 3000);
    expect(page.evaluate('moduleRan')).toBe(true);
  });

  test('syntax errors in inserted scripts are reported and do not stop the caller', () => {
    page.goto('fixtures/modules/index.html');
    page.evaluate(`window.after = 0; const s = document.createElement('script'); s.src = '../scripts/broken.js'; document.head.appendChild(s); window.after = 1;`);
    page.wait(200);
    expect(page.evaluate('after')).toBe(1);
    expect(page.console().some((m) => m.level === 'error' && m.text.includes('SyntaxError'))).toBe(true);
  });
});
