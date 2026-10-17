//! Engine conformance tests.

use super::Interp;

fn run(src: &str) -> String {
    let src = src.to_string();
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || run_inner(&src)).unwrap().join().unwrap()
}

fn run_inner(src: &str) -> String {
    let mut it = Interp::new();
    it.stack_limit = 56 << 20;
    match it.eval_script(src, "test") {
        Ok(v) => {
            it.run_microtasks();
            // allow async results stored in globals
            let out = it.eval_script("typeof __out === 'undefined' ? undefined : __out", "test").unwrap_or(v.clone());
            let shown = if out.is_undefined() { v } else { out };
            it.inspect(&shown, 4)
        }
        Err(e) => format!("THROW {}", it.describe_error(&e)),
    }
}

macro_rules! check {
    ($src:expr, $want:expr) => {
        assert_eq!(run($src), $want, "source: {}", $src);
    };
}

#[test]
fn basics() {
    check!("1 + 2 * 3", "7");
    check!("'a' + 1 + 2", "a12");
    check!("1 + 2 + 'a'", "3a");
    check!("0.1 + 0.2", "0.30000000000000004");
    check!("2 ** 10", "1024");
    check!("2 ** 3 ** 2", "512");
    check!("7 % -3", "1");
    check!("-7 >> 1", "-4");
    check!("-1 >>> 28", "15");
    check!("typeof null", "object");
    check!("typeof function(){}", "function");
    check!("typeof undeclared", "undefined");
    check!("null == undefined", "true");
    check!("'1' == 1", "true");
    check!("NaN === NaN", "false");
    check!("[1,2] + ''", "1,2");
    check!("({}) + ''", "[object Object]");
    check!("let x = 5; x += 2; x", "7");
    check!("var a = 1; { let a = 2; } a", "1");
    check!("let s = 0; for (let i = 0; i < 10; i++) { if (i === 5) continue; s += i } s", "40");
    check!("let n = 0; outer: for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { if (j == 1) continue outer; n++ } } n", "3");
    check!("let r = []; for (const k in {a:1, b:2}) r.push(k); r.join()", "a,b");
    check!("let r = 0; for (const v of [1,2,3]) r += v; r", "6");
    check!("let i = 0; do { i++ } while (i < 5); i", "5");
    check!("switch (3) { case 1: 'one'; break; case 3: 'three'; break; default: 'other' }", "three");
    check!("var out; switch (2) { case 1: out='a'; case 2: out='b'; case 3: out+='c'; break; } out", "bc");
    check!("a = 1; b = a++ + ++a; b", "4");
    check!("let o = {a: {b: null}}; o?.a?.b?.c", "undefined");
    check!("let o = null; o?.x.y.z", "undefined");
    check!("let f = null; f?.()", "undefined");
    check!("null ?? 'd'", "d");
    check!("0 || 'x'", "x");
    check!("let z = null; z ??= 4; z", "4");
    check!("`a${1+1}b${'c'}`", "a2bc");
    check!("String.raw`a\\nb`", "a\\nb");
    check!("[1, 2, 3].at(-1)", "3");
    check!("0x1f + 0b11 + 0o7 + 1_000", "1041");
    check!("10n ** 20n", "100000000000000000000n");
}

#[test]
fn functions_and_closures() {
    check!("function add(a, b = 10) { return a + b } add(1)", "11");
    check!("const f = (...xs) => xs.length; f(1,2,3)", "3");
    check!("function counter() { let c = 0; return () => ++c } const c = counter(); c(); c(); c()", "3");
    check!("const fns = []; for (let i = 0; i < 3; i++) fns.push(() => i); fns.map(f => f()).join()", "0,1,2");
    check!("function f() { return arguments.length } f(1,2,3,4)", "4");
    check!("function f() { return this } typeof f()", "object");
    check!("const o = { v: 4, get() { return this.v } }; o.get()", "4");
    check!("const o = { v: 4, f() { return [1].map(() => this.v)[0] } }; o.f()", "4");
    check!("function F(x) { this.x = x } F.prototype.get = function() { return this.x }; new F(9).get()", "9");
    check!("const b = function(x) { return this.y + x }.bind({y: 1}); b(2)", "3");
    check!("Math.max.apply(null, [1, 5, 3])", "5");
    check!("(function fact(n) { return n <= 1 ? 1 : n * fact(n - 1) })(10)", "3628800");
    check!("const f = function() {}; f.name", "f");
    check!("hoisted(); function hoisted() { return 1 }", "1");
    check!("function deep(n) { return n === 0 ? 0 : 1 + deep(n - 1) } deep(500)", "500");
    check!("function inf() { return inf() } try { inf() } catch (e) { e instanceof RangeError }", "true");
}

#[test]
fn classes() {
    check!(
        "class A { #p = 1; static s = 'S'; constructor(x) { this.x = x } get double() { return this.x * 2 } inc() { return ++this.#p } static make() { return new A(3) } }
         const a = A.make(); [a.double, a.inc(), a.inc(), A.s].join()",
        "6,2,3,S"
    );
    check!(
        "class Animal { constructor(n) { this.name = n } speak() { return this.name + ' makes a sound' } }
         class Dog extends Animal { speak() { return super.speak() + ' (woof)' } }
         new Dog('Rex').speak()",
        "Rex makes a sound (woof)"
    );
    check!("class E extends Error { constructor(m) { super(m); this.name = 'E' } } const e = new E('bad'); [e instanceof Error, e.message, String(e)].join()", "true,bad,E: bad");
    check!("class A { x = this.constructor.name } new A().x", "A");
    check!("class C { static #n = 0; static next() { return ++C.#n } } C.next(); C.next()", "2");
    check!("class P { constructor() { this.v = 1 } } class Q extends P {} new Q().v", "1");
    check!("try { class K {}; K() } catch (e) { e.constructor.name }", "TypeError");
    check!("class M extends Map { size2() { return this.size * 2 } } const m = new M([[1,2]]); m.size2()", "2");
}

#[test]
fn destructuring_and_spread() {
    check!("const {a, b: {c}, ...rest} = {a: 1, b: {c: 2}, d: 3, e: 4}; [a, c, Object.keys(rest)].join('|')", "1|2|d,e");
    check!("const [x, , y = 5, ...zs] = [1, 2, undefined, 4, 5]; [x, y, zs].join('|')", "1|5|4,5");
    check!("let a = 1, b = 2; [a, b] = [b, a]; a + ',' + b", "2,1");
    check!("function f({x = 1, y} = {}) { return x + (y || 0) } f() + f({y: 2})", "4");
    check!("Math.max(...[1, 9, 3])", "9");
    check!("({...{a: 1}, b: 2}).a", "1");
    check!("[...'héllo'].length", "5");
    check!("const o = {}; ({a: o.x} = {a: 7}); o.x", "7");
}

#[test]
fn builtins() {
    check!("[3, 1, 2].sort().join()", "1,2,3");
    check!("[10, 9, 1].sort().join()", "1,10,9");
    check!("[10, 9, 1].sort((a, b) => a - b).join()", "1,9,10");
    check!("[1, [2, [3, [4]]]].flat(Infinity).join()", "1,2,3,4");
    check!("Array.from({length: 3}, (_, i) => i * 2).join()", "0,2,4");
    check!("[1,2,3,4].filter(x => x % 2).map(x => x * 10).reduce((a, b) => a + b)", "40");
    check!("[1,2,3].includes(NaN) || [NaN].includes(NaN)", "true");
    check!("'Hello, World'.split(', ').map(s => s.toUpperCase()).join('-')", "HELLO-WORLD");
    check!("'abc'.padStart(6, '12')", "121abc");
    check!("'  x '.trim() + '|'", "x|");
    check!("'a-b-c'.replaceAll('-', '+')", "a+b+c");
    check!("'2024-01-05'.replace(/(\\d+)-(\\d+)-(\\d+)/, '$3/$2/$1')", "05/01/2024");
    check!("'aBc'.replace(/b/i, m => m.toUpperCase() + '!')", "aB!c");
    check!("'one two  three'.split(/\\s+/).length", "3");
    check!("'abc'.split('').reverse().join('')", "cba");
    check!("JSON.stringify({a: [1, 'x', null, true], b: {c: undefined}})", "{\"a\":[1,\"x\",null,true],\"b\":{}}");
    check!("JSON.parse('{\"x\": [1, 2, {\"y\": \"\\\\u00e9\"}]}').x[2].y", "é");
    check!("JSON.stringify({a: 1, b: [2]}, null, 2)", "{\n  \"a\": 1,\n  \"b\": [\n    2\n  ]\n}");
    check!("Object.entries({x: 1, y: 2}).map(([k, v]) => k + v).join()", "x1,y2");
    check!("Object.assign({}, {a: 1}, {b: 2}).b", "2");
    check!("const m = new Map([['a', 1]]); m.set('b', 2); [...m.keys()].join() + m.get('b') + m.size", "a,b22");
    check!("const s = new Set([1, 2, 2, 3]); s.size + [...s].join()", "31,2,3");
    check!("(255).toString(16) + (3.14159).toFixed(2) + parseInt('42px') + parseFloat('3.5e2x')", "ff3.1442350");
    check!("Number('0x10') + Number('') + Number(' 12 ')", "28");
    check!("Math.round(2.5) + Math.round(-2.5) + Math.floor(-1.5)", "-1");
    check!("new Date(Date.UTC(2020, 1, 29)).toISOString()", "2020-02-29T00:00:00.000Z");
    check!("/^[\\w.]+@\\w+\\.com$/.test('a.b@c.com')", "true");
    check!("'x1y22z333'.match(/\\d+/g).join()", "1,22,333");
    check!("const re = /(?<y>\\d{4})/; 'in 1999'.match(re).groups.y", "1999");
    check!("[...'a1b2'.matchAll(/[a-z](\\d)/g)].map(m => m[1]).join()", "1,2");
    check!("Symbol('x').toString()", "Symbol(x)");
    check!("const o = {[Symbol.iterator]: function* () { yield 1; yield 2 }}; [...o].join()", "1,2");
    check!("String(Object.getPrototypeOf([]) === Array.prototype)", "true");
    check!("new Uint8Array([1, 2, 300])[2]", "44");
    check!("encodeURIComponent('a b&c/é')", "a%20b%26c%2F%C3%A9");
    check!("decodeURIComponent('%E2%9C%93')", "✓");
    check!("isNaN('abc') && !isNaN('1')", "true");
    check!("Object.freeze([1]).push", "[Function: push]");
}

#[test]
fn errors() {
    check!("try { null.x } catch (e) { e.message }", "Cannot read properties of null (reading 'x')");
    check!("try { undefinedFn() } catch (e) { e.name }", "ReferenceError");
    check!("try { const c = 1; c = 2 } catch (e) { e.message }", "Assignment to constant variable.");
    check!("try { throw new TypeError('t') } catch ({name, message}) { name + ':' + message }", "TypeError:t");
    check!("let r = ''; try { r += 'a'; throw 1 } catch { r += 'b' } finally { r += 'c' } r", "abc");
    check!("function f() { try { return 'try' } finally { __out = 'fin' } } f(); __out", "fin");
    check!("JSON.parse('{bad')", "THROW SyntaxError: Expected property name or '}' in JSON at position 1".replace("Expected property name or '}' in JSON at position 1", "Unexpected token 'b' in JSON at position 1").as_str());
    check!("let x = ;", "THROW SyntaxError: Unexpected token ';' (line 1:9)");
}

#[test]
fn generators_and_async() {
    check!("function* g() { const x = yield 1; yield x * 2 } const it = g(); it.next(); it.next(21).value", "42");
    check!("function* range(n) { for (let i = 0; i < n; i++) yield i } [...range(4)].join()", "0,1,2,3");
    check!("function* inf() { let i = 0; while (true) yield i++ } const r = []; for (const v of inf()) { if (v > 3) break; r.push(v) } r.join()", "0,1,2,3");
    check!("function* a() { yield 1; yield 2 } function* b() { yield 0; yield* a(); yield 3 } [...b()].join()", "0,1,2,3");
    check!("var __out; async function f() { const a = await 1; const b = await Promise.resolve(2); return a + b } f().then(v => __out = v); undefined", "3");
    check!("var __out = []; Promise.resolve().then(() => __out.push(2)); __out.push(1); queueMicrotask(() => __out.push(3)); undefined", "[ 1, 2, 3 ]");
    check!("var __out; async function f() { throw new Error('boom') } f().catch(e => __out = e.message); undefined", "boom");
    check!("var __out; Promise.all([1, Promise.resolve(2), new Promise(r => r(3))]).then(v => __out = v.join()); undefined", "1,2,3");
    check!("var __out; Promise.allSettled([Promise.reject(1), 2]).then(v => __out = v.map(x => x.status).join()); undefined", "rejected,fulfilled");
    check!("var __out; Promise.race([new Promise(()=>{}), Promise.resolve('fast')]).then(v => __out = v); undefined", "fast");
    check!("var __out; (async () => { try { await Promise.reject(new Error('x')) } catch (e) { __out = 'caught ' + e.message } })(); undefined", "caught x");
    check!("var __out = []; async function a() { __out.push('a1'); await null; __out.push('a2') } a(); __out.push('sync'); undefined", "[ 'a1', 'sync', 'a2' ]");
    check!("var __out; (async () => { let s = 0; for await (const v of [Promise.resolve(1), 2]) s += v; __out = s })(); undefined", "3");
}

#[test]
fn getters_setters_and_misc() {
    check!("const o = { _v: 1, get v() { return this._v * 10 }, set v(x) { this._v = x } }; o.v = 5; o.v", "50");
    check!("const o = {}; Object.defineProperty(o, 'x', { get() { return 42 }, enumerable: false }); [o.x, Object.keys(o).length].join()", "42,0");
    check!("const a = [1, 2, 3]; a.length = 1; a.join()", "1");
    check!("const a = []; a[5] = 1; a.length", "6");
    check!("'abc'[1] + 'abc'.length", "b3");
    check!("let s = 0; for (const [k, v] of Object.entries({a: 1, b: 2})) s += v; s", "3");
    check!("[1, 2, 3].indexOf(2) + [1, 2, 3].findLast(x => x < 3)", "3");
    check!("new Array(3).fill(0).join('')", "000");
    check!("void 0 === undefined", "true");
    check!("delete ({a: 1}).a", "true");
    check!("'b' in {b: 1} && !('c' in {b: 1})", "true");
    check!("[] instanceof Array && !({} instanceof Array)", "true");
    check!("eval('1 + 1')", "2");
    check!("new Function('a', 'b', 'return a * b')(6, 7)", "42");
    check!("const label = `${[1,2].map(n => `<${n}>`).join('')}`; label", "<1><2>");
    check!("const x = {a: 1}; const y = structuredClone(x); y.a = 2; x.a", "1");
    check!("let i = 0; while (true) { if (++i > 100000) break } i", "100001");
}

#[test]
fn assignment_target_evaluates_before_value() {
    // Google's search page installs a method while evaluating the target object, then calls it
    // on the right-hand side: `(install(k), proto).x = k.m()`
    check!("var h = {}; (h.m = function () { return 5 }, h).n = h.m(); h.n", "5");
    check!("var log = []; function f(t, v) { log.push(t); return v } f('obj', {})[f('key', 'k')] = f('rhs', 1); log.join()", "obj,key,rhs");
    check!("var o = { a: 1 }; o.a += (o.a = 10); o.a", "11");
}
