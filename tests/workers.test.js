// Dedicated workers.

describe('workers', () => {
  test('messages, importScripts, timers and the worker global scope', () => {
    page.goto('fixtures/modules/index.html');
    page.evaluate(`
      window.got = [];
      const w = new Worker('../workers/echo.js', { name: 'echoer' });
      window.w = w;
      w.onmessage = (e) => got.push(typeof e.data === 'string' ? e.data : JSON.stringify(e.data));
      w.postMessage({ n: 21 });
      w.postMessage('env');
      w.postMessage('later');
    `);
    page.waitForFunction('got.length >= 4', 3000);
    expect(page.evaluate('got[0]')).toBe('ready');
    expect(page.evaluate('got[1]')).toBe('{"echo":{"n":21},"doubled":42}');
    expect(page.evaluate('got[2]')).toBe('{"doc":"undefined","win":"undefined","selfOk":true,"scope":true,"name":"echoer","helper":"from helper"}');
    expect(page.evaluate('got[3]')).toBe('timer fired');
    page.evaluate(`w.postMessage('bye')`);
    page.waitForFunction('got.includes("closing")', 2000);
  });

  test('workers can transfer MessagePorts to the page', () => {
    page.goto('fixtures/modules/index.html');
    page.evaluate(`
      window.res = null;
      const w = new Worker('../workers/port.js');
      w.addEventListener('message', (e) => {
        if (e.data === 'hello-port' && e.ports.length === 1) {
          const port = e.ports[0];
          port.onmessage = (m) => { res = m.data; };
          port.postMessage({ q: 14 });
        }
      });
    `);
    page.waitForFunction('res !== null', 3000);
    expect(page.evaluate('JSON.stringify(res)')).toBe('{"answer":42,"from":"worker port"}');
  });

  test('blob URL workers', () => {
    page.setContent('<p>x</p>');
    page.evaluate(`
      const src = 'onmessage = (e) => postMessage(e.data.toUpperCase());';
      const w = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
      window.reply = null;
      w.onmessage = (e) => { reply = e.data; };
      w.postMessage('shout');
    `);
    page.waitForFunction('reply !== null', 3000);
    expect(page.evaluate('reply')).toBe('SHOUT');
  });

  test('errors in a worker fire error on the Worker', () => {
    page.goto('fixtures/modules/index.html');
    page.evaluate(`window.failed = false; const w = new Worker('../workers/broken.js'); w.onerror = () => { failed = true; };`);
    page.waitForFunction('failed', 3000);
    expect(page.console().some((m) => m.text.includes('boom in worker'))).toBe(true);
  });
});
