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
