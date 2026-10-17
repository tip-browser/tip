const run = (code) => page.evaluate(`(async () => { ${code} })()`);

describe('web APIs', () => {
  beforeEach(() => page.setContent('<p id=p data-role="x">hi</p><p>two</p>'));

  test('symbol-keyed properties survive defineProperties, create, spread', () => {
    const r = page.evaluate(`(() => {
      const s = Symbol('state'); const a = [];
      Object.defineProperties(a, { [s]: { value: 5, writable: true, configurable: true } });
      a[s] |= 2;
      const o = Object.create(null, { [s]: { value: 6 } });
      return [a[s], o[s], ({ ...{ [s]: 4 } })[s], Object.getOwnPropertyDescriptors({ [s]: 7 })[s].value].join(',');
    })()`);
    expect(r).toBe('7,6,4,7');
  });

  test('Blob, File and FileReader', () => {
    run(`
      const b = new Blob(['héllo ', new Uint8Array([119, 111, 114, 108, 100])], { type: 'Text/Plain' });
      const f = new File([b], 'a.txt', { lastModified: 5 });
      const text = await b.text();
      const sliced = await b.slice(-5).text();
      const r = new FileReader();
      const url = await new Promise((res) => { r.onload = () => res(r.result); r.readAsDataURL(new Blob(['hi'], { type: 'text/plain' })); });
      document.title = [b.size, b.type, text, sliced, f.name, f.size, f instanceof Blob, url].join('|');
    `);
    page.wait(50);
    expect(page.title()).toBe('12|text/plain|héllo world|world|a.txt|12|true|data:text/plain;base64,aGk=');
  });

  test('blob: URLs can be fetched', () => {
    run(`const u = URL.createObjectURL(new Blob(['{"a":1}'], { type: 'application/json' })); const r = await fetch(u); const j = await r.json(); document.title = u.startsWith('blob:') + '|' + j.a + '|' + r.headers.get('content-type');`);
    page.wait(300);
    expect(page.title()).toBe('true|1|application/json');
  });

  test('Headers, Request and Response', () => {
    run(`
      const h = new Headers({ 'X-A': '1' }); h.append('x-a', '2'); h.set('Content-Type', 'text/plain');
      const req = new Request('/api', { method: 'post', body: 'x', headers: h });
      const res = Response.json({ ok: 1 }, { status: 201 });
      const body = await res.json();
      document.title = [h.get('x-a'), [...h.keys()].join(','), req.method, req.url.endsWith('/api'), res.status, res.ok, body.ok, res.headers.get('content-type'), res.bodyUsed].join('|');
    `);
    page.wait(50);
    expect(page.title()).toBe('1, 2|content-type,x-a|POST|true|201|true|1|application/json|true');
  });

  test('DataView reads and writes', () => {
    const r = page.evaluate(`(() => { const v = new DataView(new ArrayBuffer(16)); v.setUint16(0, 0x1234); v.setInt32(2, -2, true); v.setFloat64(8, 1.5); return [v.getUint8(0), v.getUint16(0, true), v.getInt32(2, true), v.getFloat64(8), v.byteLength].join(','); })()`);
    expect(r).toBe('18,13330,-2,1.5,16');
  });

  test('MessageChannel delivers messages asynchronously', () => {
    run(`const c = new MessageChannel(); c.port1.onmessage = (e) => { document.title = 'got ' + e.data.n; }; c.port2.postMessage({ n: 3 });`);
    page.wait(50);
    expect(page.title()).toBe('got 3');
  });

  test('interface checks', () => {
    const r = page.evaluate(`(() => {
      class Emitter extends EventTarget {}
      const e = new Emitter(); let got = 0; e.addEventListener('ping', () => got++); e.dispatchEvent(new Event('ping'));
      const ex = new DOMException('nope', 'NotFoundError');
      return [document.getElementById('p').dataset instanceof DOMStringMap, document.querySelectorAll('p') instanceof NodeList, got, ex.name, ex.code, ex instanceof Error, new DOMRect(1, 2, 3, 4).right].join(',');
    })()`);
    expect(r).toBe('true,true,1,NotFoundError,8,true,4');
  });
});

describe('AbortSignal', () => {
  test('controller signals, abort(), any() and timeout()', () => {
    page.setContent('<p>x</p>');
    page.evaluate(`(() => {
      const c = new AbortController();
      let fired = 0;
      c.signal.addEventListener('abort', () => fired++);
      c.abort(); c.abort();
      window.r1 = [c.signal instanceof AbortSignal, c.signal.aborted, c.signal.reason.name, fired, Object.prototype.toString.call(c.signal)].join();
      const a = AbortSignal.abort('why');
      let thrown; try { a.throwIfAborted(); } catch (e) { thrown = e; }
      const c2 = new AbortController();
      const any = AbortSignal.any([c2.signal, new AbortController().signal]);
      c2.abort('first');
      window.r2 = [a.aborted, thrown, any.aborted, any.reason].join();
      window.t = AbortSignal.timeout(10);
      try { new AbortSignal(); } catch (e) { window.r3 = e instanceof TypeError; }
    })()`);
    expect(page.evaluate('r1')).toBe('true,true,AbortError,1,[object AbortSignal]');
    expect(page.evaluate('r2')).toBe('true,why,true,first');
    expect(page.evaluate('r3')).toBe(true);
    page.wait(60);
    expect(page.evaluate('t.aborted + " " + t.reason.name')).toBe('true TimeoutError');
  });
});

describe('CSSOM', () => {
  test('document.styleSheets and cssRules', () => {
    page.setContent('<style id=s>/* x */ p { color: red; margin: 0 !important } @media (min-width: 1px) { .a { top: 1px } }</style><p id=p>hi</p>');
    const r = page.evaluate(`(() => {
      const s = document.styleSheets[0];
      const rules = s.cssRules;
      return [document.styleSheets.length, s.ownerNode.id, rules.length, rules[0].selectorText, rules[0].style.getPropertyValue('color'), rules[0].style.getPropertyPriority('margin'), rules[1].type, rules[1].media.mediaText, rules[1].cssRules[0].selectorText, s === document.getElementById('s').sheet, s instanceof CSSStyleSheet].join('|');
    })()`);
    expect(r).toBe('1|s|2|p|red|important|4|(min-width: 1px)|.a|true|true');
  });

  test('insertRule and deleteRule restyle the page (CSS-in-JS)', () => {
    page.setContent('<style id=s></style><div id=d class=css-1>styled</div>');
    page.evaluate(`const s = document.getElementById('s').sheet; s.insertRule('.css-1 { width: 123px; color: rgb(1, 2, 3) }', 0); s.insertRule('.css-1 { height: 7px }', 1);`);
    expect(page.box('#d').width).toBe(123);
    expect(page.box('#d').height).toBe(7);
    expect(page.evaluate('getComputedStyle(d).color')).toBe('rgb(1, 2, 3)');
    page.evaluate(`document.getElementById('s').sheet.deleteRule(0)`);
    expect(page.box('#d').width).not.toBe(123);
    expect(page.evaluate(`(() => { try { document.getElementById('s').sheet.insertRule('x { }', 9); } catch (e) { return e.name; } })()`)).toBe('IndexSizeError');
  });

  test('constructed sheets and adoptedStyleSheets', () => {
    page.setContent('<div id=d>adopted</div>');
    page.evaluate(`const sh = new CSSStyleSheet(); sh.replaceSync('#d { width: 77px }'); document.adoptedStyleSheets = [sh]; window.sh = sh;`);
    expect(page.box('#d').width).toBe(77);
    page.evaluate(`sh.insertRule('#d { width: 88px }', 1)`);
    expect(page.box('#d').width).toBe(88);
    page.evaluate(`document.adoptedStyleSheets = []`);
    expect(page.box('#d').width).not.toBe(88);
  });
});
