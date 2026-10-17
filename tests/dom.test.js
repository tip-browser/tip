// Engine integration: DOM APIs, events, timers, promises, storage and layout queries.

const html = `<!doctype html><html><head><style>
  #box { width: 200px; height: 50px; padding: 10px; border: 5px solid; margin: 7px; }
  .hidden { display: none }
  .flex { display: flex; width: 300px } .flex div { flex: 1; height: 10px }
</style></head><body>
  <div id="box">box</div>
  <ul id="list"><li>a</li><li class="x">b</li><li>c</li></ul>
  <div class="flex"><div id="f1"></div><div id="f2"></div><div id="f3" style="flex: 2"></div></div>
  <button id="btn">0</button>
  <a id="link" href="#section">jump</a>
</body></html>`;

describe('DOM & scripting', () => {
  beforeEach(() => page.setContent(html));

  test('querySelector, traversal and mutation', () => {
    const r = page.evaluate(() => {
      const list = document.getElementById('list');
      const li = document.createElement('li');
      li.textContent = 'd';
      list.appendChild(li);
      list.querySelector('.x').remove();
      return [...list.children].map(e => e.textContent).join('') + '|' + list.firstElementChild.nextElementSibling.tagName;
    });
    expect(r).toBe('acd|LI');
  });

  test('innerHTML, classList, dataset and style', () => {
    const r = page.evaluate(() => {
      const b = document.getElementById('box');
      b.innerHTML = '<span data-user-id="42">hi</span>';
      const s = b.firstChild;
      s.classList.add('a', 'b'); s.classList.toggle('a');
      s.style.color = 'red'; s.style.marginTop = '3px';
      return [s.dataset.userId, s.className, s.getAttribute('style'), b.innerHTML];
    });
    expect(r[0]).toBe('42');
    expect(r[1]).toBe('b');
    expect(r[2]).toBe('color: red; margin-top: 3px;');
    expect(r[3]).toContain('<span data-user-id="42"');
  });

  test('layout queries reflect the box model', () => {
    const r = page.evaluate(() => {
      const b = document.getElementById('box').getBoundingClientRect();
      return [b.width, b.height, document.getElementById('box').offsetWidth, getComputedStyle(document.getElementById('box')).display];
    });
    expect(r).toEqual([230, 80, 230, 'block']);
    const f = page.evaluate(() => ['f1', 'f2', 'f3'].map(id => document.getElementById(id).offsetWidth));
    expect(f).toEqual([75, 75, 150]);
  });

  test('event listeners, bubbling and preventDefault', () => {
    page.evaluate(() => {
      window.log = [];
      document.body.addEventListener('click', e => log.push('body:' + e.target.id));
      document.getElementById('btn').addEventListener('click', e => { log.push('btn'); e.currentTarget.textContent = String(+e.currentTarget.textContent + 1); });
      document.getElementById('link').addEventListener('click', e => e.preventDefault());
    });
    page.click('#btn');
    page.click('#btn');
    page.click('#link');
    expect(page.text('#btn')).toBe('2');
    expect(page.evaluate(() => log)).toEqual(['btn', 'body:btn', 'btn', 'body:btn', 'body:link']);
    expect(page.url()).not.toContain('#section');
  });

  test('timers and promises drive async updates', () => {
    page.evaluate(() => {
      window.done = [];
      setTimeout(() => done.push('timeout'), 30);
      Promise.resolve().then(() => done.push('micro'));
      (async () => { await new Promise(r => setTimeout(r, 60)); done.push('async'); })();
      const id = setInterval(() => { done.push('tick'); if (done.filter(x => x === 'tick').length === 2) clearInterval(id); }, 10);
    });
    page.wait(150);
    const done = page.evaluate(() => done);
    expect(done[0]).toBe('micro');
    expect(done).toContain('timeout');
    expect(done).toContain('async');
    expect(done.filter(x => x === 'tick').length).toBe(2);
  });

  test('evaluate awaits promises', () => {
    expect(page.evaluate('new Promise(r => setTimeout(() => r(6 * 7), 20))')).toBe(42);
  });

  test('localStorage and JSON', () => {
    const v = page.evaluate(() => {
      localStorage.setItem('k', JSON.stringify({ a: [1, 2] }));
      return JSON.parse(localStorage.getItem('k')).a.length + localStorage.length;
    });
    expect(v).toBeGreaterThanOrEqual(3);
  });

  test('MutationObserver is notified', () => {
    page.evaluate(() => {
      window.mutations = 0;
      new MutationObserver(() => mutations++).observe(document.body, { childList: true, subtree: true });
      document.body.appendChild(document.createElement('p'));
    });
    page.wait(10);
    expect(page.evaluate(() => mutations)).toBeGreaterThan(0);
  });

  test('fetch loads local files', () => {
    page.goto('../examples/todo.html');
    const len = page.evaluate(`fetch('basic.html').then(r => r.text()).then(t => t.includes('Hello, Tip!'))`);
    expect(len).toBe(true);
  });

  test('errors are reported to the console', () => {
    page.evaluate(() => { setTimeout(() => { undefinedFunction(); }, 0); });
    page.wait(20);
    expect(page.console().some(c => c.level === 'error' && c.text.includes('undefinedFunction is not defined'))).toBe(true);
  });
});
