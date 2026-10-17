// Web platform APIs implemented in JavaScript on top of the native bindings.
// Runs once per page before any page script. Keep it dependency-free and fast to parse.
(() => {
  'use strict';
  const g = globalThis;
  const define = (name, value) => Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });
  const tagOf = (v) => Object.prototype.toString.call(v).slice(8, -1);
  const enc = new TextEncoder();
  const dec = new TextDecoder();

  // ---------------------------------------------------------------- interfaces known by tag
  // Objects the engine creates natively without a JS prototype chain still answer instanceof.
  const byTag = (name, tags) => {
    const C = function () { throw new TypeError('Illegal constructor: ' + name); };
    Object.defineProperty(C, 'name', { value: name });
    Object.defineProperty(C, Symbol.hasInstance, { value: (v) => v != null && tags.includes(tagOf(v)) });
    define(name, C);
  };
  byTag('DOMStringMap', ['DOMStringMap']);
  byTag('NodeList', ['NodeList']);
  byTag('HTMLCollection', ['HTMLCollection', 'NodeList']);
  const byTagName = (name, test) => {
    if (typeof g[name] === 'function') return;
    const C = function () { throw new TypeError('Illegal constructor: ' + name); };
    Object.defineProperty(C, 'name', { value: name });
    Object.defineProperty(C, Symbol.hasInstance, { value: (v) => v != null && typeof v.tagName === 'string' && test(v.tagName.toLowerCase()) });
    define(name, C);
  };
  byTagName('HTMLMediaElement', (t) => t === 'audio' || t === 'video');
  byTagName('HTMLAudioElement', (t) => t === 'audio');
  byTagName('HTMLVideoElement', (t) => t === 'video');
  byTagName('SVGElement', (t) => ['svg', 'path', 'g', 'circle', 'rect', 'line', 'polyline', 'polygon', 'ellipse', 'use', 'text', 'defs', 'symbol'].includes(t));
  byTagName('SVGSVGElement', (t) => t === 'svg');

  // img.decode() resolves once the image has loaded
  if (typeof HTMLImageElement === 'function' && HTMLImageElement.prototype && !HTMLImageElement.prototype.decode) {
    HTMLImageElement.prototype.decode = function decode() {
      return new Promise((resolve, reject) => {
        if (this.complete) return this.naturalWidth ? resolve() : reject(new DOMException('The source image cannot be decoded.', 'EncodingError'));
        this.addEventListener('load', () => resolve(), { once: true });
        this.addEventListener('error', () => reject(new DOMException('The source image cannot be decoded.', 'EncodingError')), { once: true });
      });
    };
  }

  if (typeof g.Audio !== 'function') {
    define('Audio', function Audio(src) {
      const a = document.createElement('audio');
      if (src !== undefined) a.src = src;
      return a;
    });
  }

  // ---------------------------------------------------------------- DOMException
  if (typeof g.DOMException !== 'function') {
    const codes = { IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4, InvalidCharacterError: 5, NoModificationAllowedError: 7, NotFoundError: 8, NotSupportedError: 9, InvalidStateError: 11, SyntaxError: 12, InvalidModificationError: 13, NamespaceError: 14, InvalidAccessError: 15, TypeMismatchError: 17, SecurityError: 18, NetworkError: 19, AbortError: 20, URLMismatchError: 21, QuotaExceededError: 22, TimeoutError: 23, InvalidNodeTypeError: 24, DataCloneError: 25 };
    class DOMException extends Error {
      constructor(message = '', name = 'Error') {
        super(String(message));
        Object.defineProperty(this, 'name', { value: String(name), configurable: true, writable: true });
      }
      get code() { return codes[this.name] || 0; }
    }
    for (const [k, v] of Object.entries(codes)) {
      const key = k.replace(/([a-z])([A-Z])/g, '$1_$2').toUpperCase().replace(/_ERROR$/, '_ERR');
      DOMException[key] = v;
    }
    define('DOMException', DOMException);
  }

  // ---------------------------------------------------------------- DOMRect
  if (typeof g.DOMRect !== 'function') {
    class DOMRectReadOnly {
      constructor(x = 0, y = 0, width = 0, height = 0) { this._x = +x; this._y = +y; this._w = +width; this._h = +height; }
      get x() { return this._x; } get y() { return this._y; }
      get width() { return this._w; } get height() { return this._h; }
      get left() { return Math.min(this._x, this._x + this._w); } get right() { return Math.max(this._x, this._x + this._w); }
      get top() { return Math.min(this._y, this._y + this._h); } get bottom() { return Math.max(this._y, this._y + this._h); }
      toJSON() { const { x, y, width, height, top, right, bottom, left } = this; return { x, y, width, height, top, right, bottom, left }; }
      static fromRect(r = {}) { return new this(r.x, r.y, r.width, r.height); }
    }
    class DOMRect extends DOMRectReadOnly {
      set x(v) { this._x = +v; } set y(v) { this._y = +v; } set width(v) { this._w = +v; } set height(v) { this._h = +v; }
      get x() { return this._x; } get y() { return this._y; } get width() { return this._w; } get height() { return this._h; }
    }
    define('DOMRectReadOnly', DOMRectReadOnly);
    define('DOMRect', DOMRect);
  }

  // ---------------------------------------------------------------- DataView
  if (typeof g.DataView !== 'function') {
    const f32 = new Float32Array(1), f32b = new Uint8Array(f32.buffer);
    const f64 = new Float64Array(1), f64b = new Uint8Array(f64.buffer);
    class DataView {
      constructor(buffer, byteOffset = 0, byteLength) {
        if (!(buffer instanceof ArrayBuffer)) throw new TypeError('First argument to DataView constructor must be an ArrayBuffer');
        this.buffer = buffer; this.byteOffset = byteOffset;
        this.byteLength = byteLength === undefined ? buffer.byteLength - byteOffset : byteLength;
        this._u8 = new Uint8Array(buffer, byteOffset, this.byteLength);
      }
      _get(o, n, le) { if (o < 0 || o + n > this.byteLength) throw new RangeError('Offset is outside the bounds of the DataView'); const b = []; for (let i = 0; i < n; i++) b.push(this._u8[o + i]); return le ? b : b.reverse(); }
      _set(o, bytes, le) { if (o < 0 || o + bytes.length > this.byteLength) throw new RangeError('Offset is outside the bounds of the DataView'); const b = le ? bytes : bytes.slice().reverse(); for (let i = 0; i < b.length; i++) this._u8[o + i] = b[i]; }
      _uint(o, n, le) { const b = this._get(o, n, le); let v = 0; for (let i = n - 1; i >= 0; i--) v = v * 256 + b[i]; return v; }
      _putUint(o, n, v, le) { const b = []; for (let i = 0; i < n; i++) { b.push(v & 255); v = Math.floor(v / 256); } this._set(o, b, le); }
      getUint8(o) { return this._uint(o, 1, true); } getInt8(o) { const v = this.getUint8(o); return v > 127 ? v - 256 : v; }
      getUint16(o, le) { return this._uint(o, 2, le); } getInt16(o, le) { const v = this._uint(o, 2, le); return v > 32767 ? v - 65536 : v; }
      getUint32(o, le) { return this._uint(o, 4, le); } getInt32(o, le) { return this._uint(o, 4, le) | 0; }
      getFloat32(o, le) { const b = this._get(o, 4, le); for (let i = 0; i < 4; i++) f32b[i] = b[i]; return f32[0]; }
      getFloat64(o, le) { const b = this._get(o, 8, le); for (let i = 0; i < 8; i++) f64b[i] = b[i]; return f64[0]; }
      getBigUint64(o, le) { const lo = BigInt(this.getUint32(o + (le ? 0 : 4), le)), hi = BigInt(this.getUint32(o + (le ? 4 : 0), le)); return (hi << 32n) | lo; }
      getBigInt64(o, le) { const v = this.getBigUint64(o, le); return v >= (1n << 63n) ? v - (1n << 64n) : v; }
      setUint8(o, v) { this._putUint(o, 1, v >>> 0 & 255, true); } setInt8(o, v) { this.setUint8(o, v); }
      setUint16(o, v, le) { this._putUint(o, 2, v & 65535, le); } setInt16(o, v, le) { this.setUint16(o, v, le); }
      setUint32(o, v, le) { this._putUint(o, 4, v >>> 0, le); } setInt32(o, v, le) { this.setUint32(o, v, le); }
      setFloat32(o, v, le) { f32[0] = v; this._set(o, Array.from(f32b), le); }
      setFloat64(o, v, le) { f64[0] = v; this._set(o, Array.from(f64b), le); }
      setBigUint64(o, v, le) { v = BigInt.asUintN(64, BigInt(v)); const lo = Number(v & 0xffffffffn), hi = Number(v >> 32n); this.setUint32(o + (le ? 0 : 4), lo, le); this.setUint32(o + (le ? 4 : 0), hi, le); }
      setBigInt64(o, v, le) { this.setBigUint64(o, v, le); }
      get [Symbol.toStringTag]() { return 'DataView'; }
    }
    define('DataView', DataView);
  }

  // ---------------------------------------------------------------- Blob / File / FileReader
  const toBytes = (part) => {
    if (part instanceof Blob) return part.__tipBytes();
    if (part instanceof ArrayBuffer) return new Uint8Array(part.slice(0));
    if (ArrayBuffer.isView(part)) return new Uint8Array(part.buffer.slice(part.byteOffset, part.byteOffset + part.byteLength));
    return enc.encode(String(part));
  };
  class Blob {
    #bytes; #type;
    constructor(parts = [], options = {}) {
      const chunks = [];
      for (const p of parts) chunks.push(toBytes(p));
      const total = chunks.reduce((n, c) => n + c.length, 0);
      const bytes = new Uint8Array(total);
      let at = 0;
      for (const c of chunks) { bytes.set(c, at); at += c.length; }
      this.#bytes = bytes;
      const t = options && options.type !== undefined ? String(options.type) : '';
      this.#type = /^[\x20-\x7e]*$/.test(t) ? t.toLowerCase() : '';
    }
    get size() { return this.#bytes.length; }
    get type() { return this.#type; }
    __tipBytes() { return this.#bytes; }
    text() { return Promise.resolve(dec.decode(this.#bytes)); }
    arrayBuffer() { return Promise.resolve(this.#bytes.slice().buffer); }
    bytes() { return Promise.resolve(this.#bytes.slice()); }
    slice(start = 0, end = this.size, type = '') {
      const n = this.size;
      const s = start < 0 ? Math.max(n + start, 0) : Math.min(start, n);
      const e = end < 0 ? Math.max(n + end, 0) : Math.min(end, n);
      return new Blob([this.#bytes.slice(s, Math.max(s, e))], { type });
    }
    stream() {
      const bytes = this.#bytes;
      if (typeof ReadableStream === 'function') return new ReadableStream({ start(c) { c.enqueue(bytes.slice()); c.close(); } });
      throw new TypeError('ReadableStream is not supported');
    }
    get [Symbol.toStringTag]() { return 'Blob'; }
  }
  Object.defineProperty(Blob.prototype, '__tipBytes', { enumerable: false });
  class File extends Blob {
    #name; #lastModified;
    constructor(parts, name, options = {}) {
      super(parts, options);
      this.#name = String(name);
      this.#lastModified = options && options.lastModified !== undefined ? Number(options.lastModified) : Date.now();
    }
    get name() { return this.#name; }
    get lastModified() { return this.#lastModified; }
    get webkitRelativePath() { return ''; }
    get [Symbol.toStringTag]() { return 'File'; }
  }
  define('Blob', Blob);
  define('File', File);

  const base64 = (bytes) => {
    let s = '';
    for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(s);
  };

  class FileReader extends EventTarget {
    constructor() {
      super();
      this.readyState = 0; this.result = null; this.error = null;
      this.onload = this.onloadstart = this.onloadend = this.onerror = this.onprogress = this.onabort = null;
    }
    _read(blob, how, arg) {
      if (!(blob instanceof Blob)) throw new TypeError("Failed to execute 'read' on 'FileReader': parameter 1 is not of type 'Blob'.");
      if (this.readyState === 1) throw new DOMException('The object is already busy reading Blobs.', 'InvalidStateError');
      this.readyState = 1; this.result = null;
      const fire = (type) => this.dispatchEvent(new ProgressEvent(type, { lengthComputable: true, loaded: blob.size, total: blob.size }));
      setTimeout(() => {
        fire('loadstart');
        const bytes = blob.__tipBytes();
        if (how === 'text') this.result = new TextDecoder(arg || 'utf-8').decode(bytes);
        else if (how === 'dataurl') this.result = 'data:' + (blob.type || 'application/octet-stream') + ';base64,' + base64(bytes);
        else if (how === 'binary') this.result = Array.from(bytes, (b) => String.fromCharCode(b)).join('');
        else this.result = bytes.slice().buffer;
        this.readyState = 2;
        fire('progress'); fire('load'); fire('loadend');
      }, 0);
    }
    readAsText(b, encoding) { this._read(b, 'text', encoding); }
    readAsDataURL(b) { this._read(b, 'dataurl'); }
    readAsArrayBuffer(b) { this._read(b, 'buffer'); }
    readAsBinaryString(b) { this._read(b, 'binary'); }
    abort() { this.readyState = 2; this.result = null; }
  }
  FileReader.EMPTY = 0; FileReader.LOADING = 1; FileReader.DONE = 2;
  define('FileReader', FileReader);

  // blob: URLs are served by the network layer from a process-wide store
  if (typeof URL === 'function') {
    URL.createObjectURL = function createObjectURL(obj) {
      if (!(obj instanceof Blob)) throw new TypeError("Failed to execute 'createObjectURL' on 'URL': Overload resolution failed.");
      const origin = location.origin && location.origin !== 'null' ? location.origin : 'null';
      const url = 'blob:' + origin + '/' + crypto.randomUUID();
      __tip_blob_register(url, obj.__tipBytes(), obj.type);
      return url;
    };
    URL.revokeObjectURL = function revokeObjectURL(url) { __tip_blob_revoke(String(url)); };
  }

  // ---------------------------------------------------------------- Headers / Request / Response
  const normName = (n) => {
    n = String(n);
    if (!/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(n)) throw new TypeError(`Invalid name: ${n}`);
    return n.toLowerCase();
  };
  class Headers {
    #map = new Map();
    constructor(init) {
      if (init == null) return;
      if (init instanceof Headers) { for (const [k, v] of init) this.append(k, v); return; }
      if (typeof init[Symbol.iterator] === 'function') { for (const pair of init) { const [k, v] = pair; this.append(k, v); } return; }
      for (const k of Object.keys(init)) this.append(k, init[k]);
    }
    append(n, v) { n = normName(n); v = String(v).trim(); const cur = this.#map.get(n); this.#map.set(n, cur === undefined ? v : (n === 'set-cookie' ? cur + '\n' + v : cur + ', ' + v)); }
    set(n, v) { this.#map.set(normName(n), String(v).trim()); }
    get(n) { const v = this.#map.get(normName(n)); return v === undefined ? null : v; }
    getSetCookie() { const v = this.#map.get('set-cookie'); return v === undefined ? [] : v.split('\n'); }
    has(n) { return this.#map.has(normName(n)); }
    delete(n) { this.#map.delete(normName(n)); }
    forEach(cb, thisArg) { for (const [k, v] of this) cb.call(thisArg, v, k, this); }
    *entries() { yield* [...this.#map.entries()].sort((a, b) => (a[0] < b[0] ? -1 : 1)); }
    *keys() { for (const [k] of this.entries()) yield k; }
    *values() { for (const [, v] of this.entries()) yield v; }
    [Symbol.iterator]() { return this.entries(); }
    get [Symbol.toStringTag]() { return 'Headers'; }
  }
  define('Headers', Headers);

  const bodyBytes = (body) => {
    if (body == null) return null;
    if (typeof body === 'string') return enc.encode(body);
    if (body instanceof Blob) return body.__tipBytes();
    if (body instanceof ArrayBuffer) return new Uint8Array(body);
    if (ArrayBuffer.isView(body)) return new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
    if (body instanceof URLSearchParams) return enc.encode(body.toString());
    if (typeof body.__tipSerialize === 'function') return enc.encode(body.__tipSerialize());
    return enc.encode(String(body));
  };
  const defaultType = (body) => {
    if (typeof body === 'string') return 'text/plain;charset=UTF-8';
    if (body instanceof Blob) return body.type || null;
    if (body instanceof URLSearchParams || (body && typeof body.__tipSerialize === 'function')) return 'application/x-www-form-urlencoded;charset=UTF-8';
    return null;
  };
  const bodyMixin = (proto) => {
    Object.assign(proto, {
      _consume() {
        if (this.bodyUsed) return Promise.reject(new TypeError('Body is unusable: Body has already been read'));
        this._used = true;
        return Promise.resolve(this._bytes || new Uint8Array(0));
      },
      text() { return this._consume().then((b) => dec.decode(b)); },
      json() { return this.text().then((t) => JSON.parse(t)); },
      arrayBuffer() { return this._consume().then((b) => b.slice().buffer); },
      bytes() { return this._consume().then((b) => b.slice()); },
      blob() { return this._consume().then((b) => new Blob([b], { type: this.headers.get('content-type') || '' })); },
      formData() { return this.text().then((t) => { const f = new FormData(); for (const [k, v] of new URLSearchParams(t)) f.append(k, v); return f; }); },
    });
    Object.defineProperty(proto, 'bodyUsed', { get() { return !!this._used; }, configurable: true });
    Object.defineProperty(proto, 'body', { get() { return this._bytes ? this._bytes : null; }, configurable: true });
  };

  class Request {
    constructor(input, init = {}) {
      const src = input instanceof Request ? input : null;
      this.url = src ? src.url : new URL(String(input), location.href).href;
      this.method = String(init.method || (src && src.method) || 'GET').toUpperCase();
      this.headers = new Headers(init.headers || (src && src.headers) || undefined);
      const body = init.body !== undefined ? init.body : src ? src._bytes : null;
      if (body != null && (this.method === 'GET' || this.method === 'HEAD')) throw new TypeError("Failed to construct 'Request': Request with GET/HEAD method cannot have body.");
      this._bytes = bodyBytes(body);
      const ct = defaultType(body);
      if (ct && !this.headers.has('content-type')) this.headers.set('content-type', ct);
      this.credentials = init.credentials || (src && src.credentials) || 'same-origin';
      this.mode = init.mode || (src && src.mode) || 'cors';
      this.cache = init.cache || 'default';
      this.redirect = init.redirect || 'follow';
      this.referrer = 'about:client';
      this.signal = init.signal || (src && src.signal) || null;
      this.keepalive = !!init.keepalive;
    }
    clone() { return new Request(this); }
    get [Symbol.toStringTag]() { return 'Request'; }
  }
  bodyMixin(Request.prototype);
  define('Request', Request);

  class Response {
    constructor(body = null, init = {}) {
      this.status = init.status === undefined ? 200 : init.status | 0;
      if (this.status < 200 || this.status > 599) throw new RangeError("Failed to construct 'Response': The status provided (" + this.status + ') is outside the range [200, 599].');
      this.statusText = init.statusText === undefined ? '' : String(init.statusText);
      this.headers = new Headers(init.headers);
      this._bytes = bodyBytes(body);
      const ct = defaultType(body);
      if (ct && !this.headers.has('content-type')) this.headers.set('content-type', ct);
      this.type = 'default'; this.url = ''; this.redirected = false;
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    clone() { if (this.bodyUsed) throw new TypeError('Response body is already used'); const r = new Response(this._bytes, this); r.type = this.type; r.url = this.url; return r; }
    static json(data, init = {}) { const h = new Headers(init.headers); if (!h.has('content-type')) h.set('content-type', 'application/json'); return new Response(JSON.stringify(data), { ...init, headers: h }); }
    static error() { const r = new Response(null, { status: 200 }); r.status = 0; r.type = 'error'; return r; }
    static redirect(url, status = 302) { return new Response(null, { status, headers: { location: new URL(url, location.href).href } }); }
    get [Symbol.toStringTag]() { return 'Response'; }
  }
  bodyMixin(Response.prototype);
  define('Response', Response);

  // fetch accepts Request objects, Headers and binary/Blob bodies; native responses gain the full interface
  const nativeFetch = g.fetch;
  if (typeof nativeFetch === 'function') {
    define('fetch', function fetch(input, init) {
      let url = input, opts = init ? { ...init } : {};
      if (input instanceof Request) {
        url = input.url;
        opts = { method: input.method, headers: input.headers, body: input._bytes, credentials: input.credentials, signal: input.signal, ...opts };
      } else if (input && typeof input === 'object' && 'href' in input) {
        url = input.href;
      }
      if (opts.headers instanceof Headers) {
        const h = {};
        for (const [k, v] of opts.headers) h[k] = v;
        opts.headers = h;
      }
      if (opts.body instanceof Blob) {
        const type = opts.body.type;
        opts.body = opts.body.__tipBytes();
        if (type) opts.headers = { 'content-type': type, ...(opts.headers || {}) };
      }
      return nativeFetch.call(this, url, opts);
    });
  }

  // ---------------------------------------------------------------- messaging
  if (typeof g.MessageChannel !== 'function') {
    class MessagePort extends EventTarget {
      constructor() { super(); this._other = null; this._open = true; this.onmessage = null; this.onmessageerror = null; this._id = 0; this._remote = 0; }
      get __tipPort() { return true; }
      postMessage(data) {
        if (this._remote) {
          // the other end lives in another window
          if (this._open) __tip_port_send(this._remote, data);
          return;
        }
        const to = this._other;
        if (!to || !to._open || !this._open) return;
        const copy = typeof structuredClone === 'function' ? structuredClone(data) : data;
        setTimeout(() => { if (to._open) to.dispatchEvent(new MessageEvent('message', { data: copy })); }, 0);
      }
      start() {}
      close() { this._open = false; }
      get [Symbol.toStringTag]() { return 'MessagePort'; }
    }
    class MessageChannel {
      constructor() { this.port1 = new MessagePort(); this.port2 = new MessagePort(); this.port1._other = this.port2; this.port2._other = this.port1; }
    }
    define('MessagePort', MessagePort);
    define('MessageChannel', MessageChannel);
  }
  Object.defineProperty(g, '__tip_port_deliver', {
    value: (port, data) => { if (port._open) port.dispatchEvent(new MessageEvent('message', { data })); },
    configurable: true,
  });
  if (typeof g.BroadcastChannel !== 'function') {
    const channels = new Map();
    class BroadcastChannel extends EventTarget {
      constructor(name) { super(); this.name = String(name); this.onmessage = null; this._open = true; if (!channels.has(this.name)) channels.set(this.name, new Set()); channels.get(this.name).add(this); }
      postMessage(data) {
        if (!this._open) throw new DOMException('Channel is closed', 'InvalidStateError');
        for (const c of channels.get(this.name)) if (c !== this) setTimeout(() => c._open && c.dispatchEvent(new MessageEvent('message', { data: structuredClone(data) })), 0);
      }
      close() { this._open = false; channels.get(this.name).delete(this); }
    }
    define('BroadcastChannel', BroadcastChannel);
  }

  // ---------------------------------------------------------------- observers and permissions
  if (typeof g.PerformanceObserver !== 'function') {
    class PerformanceObserver {
      constructor(cb) { this._cb = cb; }
      observe() {} disconnect() {} takeRecords() { return []; }
      static get supportedEntryTypes() { return []; }
    }
    define('PerformanceObserver', PerformanceObserver);
  }
  if (typeof g.Notification !== 'function') {
    class Notification extends EventTarget {
      constructor(title, options = {}) { super(); this.title = String(title); Object.assign(this, options); setTimeout(() => this.dispatchEvent(new Event('error')), 0); }
      close() {}
      static get permission() { return 'default'; }
      static requestPermission(cb) { const p = Promise.resolve('denied'); if (typeof cb === 'function') p.then(cb); return p; }
    }
    define('Notification', Notification);
  }
  if (typeof navigator === 'object') {
    const nav = navigator;
    const navDefine = (k, v) => { if (nav[k] === undefined) Object.defineProperty(nav, k, { value: v, configurable: true }); };
    navDefine('permissions', { query: (d) => Promise.resolve({ name: d && d.name, state: 'prompt', onchange: null, addEventListener() {}, removeEventListener() {} }) });
    navDefine('storage', { estimate: () => Promise.resolve({ quota: 1 << 30, usage: 0 }), persist: () => Promise.resolve(false), persisted: () => Promise.resolve(false) });
  }
  if (typeof g.XMLSerializer !== 'function') {
    class XMLSerializer { serializeToString(node) { return node.nodeType === 9 ? node.documentElement.outerHTML : node.nodeType === 1 ? node.outerHTML : node.textContent; } }
    define('XMLSerializer', XMLSerializer);
  }
})();

// AbortSignal: the type of AbortController#signal, plus its static helpers
(() => {
  function AbortSignal() { throw new TypeError('Illegal constructor'); }
  AbortSignal.prototype = Object.create(EventTarget.prototype, { constructor: { value: AbortSignal, writable: true, configurable: true } });
  Object.defineProperty(AbortSignal.prototype, Symbol.toStringTag, { value: 'AbortSignal', configurable: true });
  AbortSignal.prototype.throwIfAborted = function () { if (this.aborted) throw this.reason; };
  AbortSignal.abort = function (reason) {
    const c = new AbortController();
    c.abort(reason);
    return c.signal;
  };
  AbortSignal.timeout = function (ms) {
    const c = new AbortController();
    setTimeout(() => c.abort(new DOMException('signal timed out', 'TimeoutError')), Number(ms) || 0);
    return c.signal;
  };
  AbortSignal.any = function (signals) {
    const c = new AbortController();
    for (const s of signals) {
      if (s.aborted) { c.abort(s.reason); return c.signal; }
    }
    for (const s of signals) s.addEventListener('abort', () => c.abort(s.reason));
    return c.signal;
  };
  Object.defineProperty(globalThis, 'AbortSignal', { value: AbortSignal, writable: true, configurable: true });
})();

// CSSOM: document.styleSheets, element.sheet, CSSStyleSheet with insertRule/deleteRule,
// constructed sheets and document.adoptedStyleSheets
(() => {
  const g = globalThis;
  const define = (name, value) => Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });
  const RULE_TYPES = { style: 1, import: 3, media: 4, 'font-face': 5, page: 6, keyframes: 7, namespace: 10, supports: 12, layer: 18, container: 19 };
  const parseDecls = (body) => {
    const out = [];
    for (const part of body.split(';')) {
      const i = part.indexOf(':');
      if (i > 0) out.push([part.slice(0, i).trim().toLowerCase(), part.slice(i + 1).trim()]);
    }
    return out;
  };
  class CSSRuleStyle {
    constructor(rule, body) { Object.defineProperty(this, '_decls', { value: parseDecls(body), writable: true }); }
    get length() { return this._decls.length; }
    item(i) { return this._decls[i] ? this._decls[i][0] : ''; }
    getPropertyValue(p) { const d = this._decls.find((x) => x[0] === String(p).toLowerCase()); return d ? d[1].replace(/\s*!important\s*$/i, '') : ''; }
    getPropertyPriority(p) { const d = this._decls.find((x) => x[0] === String(p).toLowerCase()); return d && /!important\s*$/i.test(d[1]) ? 'important' : ''; }
    get cssText() { return this._decls.map(([k, v]) => k + ': ' + v + ';').join(' '); }
  }
  class CSSRule {
    constructor(text, sheet, parent) {
      Object.defineProperties(this, { _text: { value: text }, parentStyleSheet: { value: sheet || null }, parentRule: { value: parent || null } });
      const at = /^@([-\w]+)/.exec(text);
      const kind = at ? at[1].replace(/^-\w+-/, '').toLowerCase() : 'style';
      Object.defineProperty(this, 'type', { value: RULE_TYPES[kind] || 0 });
      const brace = text.indexOf('{');
      const head = brace >= 0 ? text.slice(0, brace).trim() : text.replace(/;$/, '').trim();
      const body = brace >= 0 ? text.slice(brace + 1, text.lastIndexOf('}')) : '';
      if (kind === 'style') {
        this.selectorText = head;
        this.style = new CSSRuleStyle(this, body);
      } else if (kind === 'media' || kind === 'supports' || kind === 'container' || kind === 'layer') {
        const cond = head.replace(/^@[-\w]+\s*/, '');
        if (kind === 'media') this.media = { mediaText: cond, length: cond ? cond.split(',').length : 0, toString() { return cond; } };
        else this.conditionText = cond;
        const inner = __tip_css_split(body);
        this.cssRules = makeRuleList(inner.map((t) => new CSSRule(t, sheet, this)));
      } else if (kind === 'keyframes') {
        this.name = head.replace(/^@[-\w]+\s*/, '');
        this.cssRules = makeRuleList(__tip_css_split(body).map((t) => new CSSRule(t, sheet, this)));
      } else if (kind === 'font-face' || kind === 'page') {
        this.style = new CSSRuleStyle(this, body);
      } else if (kind === 'import') {
        const m = /url\(\s*['"]?([^'")]+)/.exec(head) || /['"]([^'"]+)['"]/.exec(head);
        this.href = m ? m[1] : '';
      }
    }
    get cssText() { return this._text; }
  }
  const makeRuleList = (rules) => {
    const list = Object.create(CSSRuleList.prototype);
    rules.forEach((r, i) => { list[i] = r; });
    Object.defineProperty(list, 'length', { value: rules.length });
    return list;
  };
  function CSSRuleList() { throw new TypeError('Illegal constructor'); }
  CSSRuleList.prototype.item = function (i) { return this[i] || null; };
  CSSRuleList.prototype[Symbol.iterator] = function* () { for (let i = 0; i < this.length; i++) yield this[i]; };

  let nextId = 1;
  class CSSStyleSheet {
    constructor(options) {
      Object.defineProperty(this, '_id', { value: nextId++ });
      Object.defineProperty(this, '_owner', { value: null, writable: true });
      Object.defineProperty(this, '_list', { value: [], writable: true });
      const media = options && options.media ? String(options.media) : '';
      this.media = { mediaText: media, length: media ? 1 : 0, toString() { return media; } };
      this.disabled = !!(options && options.disabled);
    }
    _texts() { return this._owner ? (__tip_sheet_rules(this._owner) || []) : this._list.slice(); }
    _commit(list) { if (this._owner) __tip_sheet_set(this._owner, list); else { this._list = list; __tip_csheet_set(this._id, list); } }
    get ownerNode() { return this._owner; }
    get href() {
      const o = this._owner;
      return o && o.localName === 'link' ? o.href : null;
    }
    get type() { return 'text/css'; }
    get title() { return this._owner ? this._owner.getAttribute('title') : null; }
    get parentStyleSheet() { return null; }
    get ownerRule() { return null; }
    get cssRules() { return makeRuleList(this._texts().map((t) => new CSSRule(t, this))); }
    get rules() { return this.cssRules; }
    insertRule(rule, index) {
      const list = this._texts();
      const i = index === undefined ? 0 : Number(index) >>> 0;
      if (i > list.length) throw new DOMException(`Failed to execute 'insertRule' on 'CSSStyleSheet': The index provided (${i}) is larger than the maximum index (${list.length}).`, 'IndexSizeError');
      const parsed = __tip_css_split(String(rule));
      if (parsed.length !== 1) throw new DOMException(`Failed to execute 'insertRule' on 'CSSStyleSheet': Failed to parse the rule '${rule}'.`, 'SyntaxError');
      list.splice(i, 0, parsed[0]);
      this._commit(list);
      return i;
    }
    deleteRule(index) {
      const list = this._texts();
      const i = Number(index) >>> 0;
      if (i >= list.length) throw new DOMException(`Failed to execute 'deleteRule' on 'CSSStyleSheet': The index provided (${i}) is larger than the maximum index (${list.length - 1}).`, 'IndexSizeError');
      list.splice(i, 1);
      this._commit(list);
    }
    addRule(selector, style, index) {
      const len = this._texts().length;
      this.insertRule(`${selector} { ${style} }`, index === undefined ? len : index);
      return -1;
    }
    removeRule(index) { this.deleteRule(index === undefined ? 0 : index); }
    replaceSync(text) { this._commit(__tip_css_split(String(text))); }
    replace(text) { try { this.replaceSync(text); return Promise.resolve(this); } catch (e) { return Promise.reject(e); } }
    get [Symbol.toStringTag]() { return 'CSSStyleSheet'; }
  }
  const sheets = new WeakMap();
  const sheetOf = (el) => {
    if (el.localName === 'link' && !/(^|\s)stylesheet(\s|$)/i.test(el.getAttribute('rel') || '')) return null;
    if (!el.isConnected) return null;
    let s = sheets.get(el);
    if (!s) {
      s = new CSSStyleSheet();
      s._owner = el;
      const media = el.getAttribute('media') || '';
      s.media = { mediaText: media, length: media ? 1 : 0, toString() { return media; } };
      sheets.set(el, s);
    }
    return s;
  };
  for (const C of [g.HTMLStyleElement, g.HTMLLinkElement]) {
    if (C && C.prototype) Object.defineProperty(C.prototype, 'sheet', { get() { return sheetOf(this); }, configurable: true });
  }
  function StyleSheetList() { throw new TypeError('Illegal constructor'); }
  StyleSheetList.prototype.item = function (i) { return this[i] || null; };
  StyleSheetList.prototype[Symbol.iterator] = function* () { for (let i = 0; i < this.length; i++) yield this[i]; };
  Object.defineProperty(Document.prototype, 'styleSheets', {
    get() {
      const list = Object.create(StyleSheetList.prototype);
      let n = 0;
      for (const el of this.querySelectorAll('style, link[rel~="stylesheet" i]')) {
        const s = sheetOf(el);
        if (s) list[n++] = s;
      }
      Object.defineProperty(list, 'length', { value: n });
      return list;
    },
    configurable: true,
  });
  let adopted = Object.freeze([]);
  Object.defineProperty(Document.prototype, 'adoptedStyleSheets', {
    get() { return adopted; },
    set(v) {
      const arr = Array.from(v);
      for (const s of arr) if (!(s instanceof CSSStyleSheet) || s._owner) throw new DOMException("Failed to set the 'adoptedStyleSheets' property on 'Document': Can't adopt non-constructed stylesheets.", 'NotAllowedError');
      adopted = Object.freeze(arr);
      __tip_adopt(arr.map((s) => s._id));
    },
    configurable: true,
  });
  define('CSSStyleSheet', CSSStyleSheet);
  define('StyleSheet', CSSStyleSheet);
  define('CSSRule', CSSRule);
  define('CSSStyleRule', CSSRule);
  define('CSSMediaRule', CSSRule);
  define('CSSRuleList', CSSRuleList);
  define('StyleSheetList', StyleSheetList);
})();

// Attr and NamedNodeMap (element.attributes)
(() => {
  const g = globalThis;
  const define = (name, value) => Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });
  function Attr() { throw new TypeError('Illegal constructor'); }
  Object.defineProperties(Attr.prototype, {
    value: {
      get() { return this.ownerElement ? (this.ownerElement.getAttribute(this.name) ?? this._value) : this._value; },
      set(v) { this._value = String(v); if (this.ownerElement) this.ownerElement.setAttribute(this.name, this._value); },
      configurable: true,
    },
    nodeValue: { get() { return this.value; }, set(v) { this.value = v; }, configurable: true },
    textContent: { get() { return this.value; }, set(v) { this.value = v; }, configurable: true },
    specified: { get() { return true; }, configurable: true },
    namespaceURI: { get() { return null; }, configurable: true },
    prefix: { get() { return null; }, configurable: true },
    nodeType: { get() { return 2; }, configurable: true },
    [Symbol.toStringTag]: { value: 'Attr', configurable: true },
  });
  function NamedNodeMap() { throw new TypeError('Illegal constructor'); }
  Object.assign(NamedNodeMap.prototype, {
    item(i) { return this[i] || null; },
    getNamedItem(name) {
      const n = String(name).toLowerCase();
      for (let i = 0; i < this.length; i++) if (this[i].name === n) return this[i];
      return null;
    },
    getNamedItemNS(ns, name) { return this.getNamedItem(name); },
    setNamedItem(attr) { const old = this.getNamedItem(attr.name); this._owner.setAttribute(attr.name, attr.value); return old; },
    removeNamedItem(name) {
      const old = this.getNamedItem(name);
      if (!old) throw new DOMException(`Failed to execute 'removeNamedItem' on 'NamedNodeMap': No item with name '${name}' was found.`, 'NotFoundError');
      this._owner.removeAttribute(name);
      return old;
    },
    removeNamedItemNS(ns, name) { return this.removeNamedItem(name); },
  });
  NamedNodeMap.prototype[Symbol.iterator] = function* () { for (let i = 0; i < this.length; i++) yield this[i]; };
  Object.defineProperty(NamedNodeMap.prototype, Symbol.toStringTag, { value: 'NamedNodeMap', configurable: true });
  define('Attr', Attr);
  define('NamedNodeMap', NamedNodeMap);
  // document.createAttribute and element.getAttributeNode / setAttributeNode
  if (!Document.prototype.createAttribute) {
    Document.prototype.createAttribute = function (name) {
      const a = Object.create(Attr.prototype);
      Object.defineProperties(a, { name: { value: String(name).toLowerCase() }, localName: { value: String(name).toLowerCase() }, nodeName: { value: String(name).toLowerCase() }, ownerElement: { value: null, writable: true }, _value: { value: '', writable: true } });
      return a;
    };
  }
  Element.prototype.getAttributeNode = function (name) { return this.attributes.getNamedItem(name); };
  Element.prototype.setAttributeNode = function (attr) { const old = this.getAttributeNode(attr.name); this.setAttribute(attr.name, attr.value); return old; };
  Element.prototype.removeAttributeNode = function (attr) { this.removeAttribute(attr.name); return attr; };
})();
