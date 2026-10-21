describe('ES modules', () => {
  beforeEach(() => page.goto('fixtures/modules/index.html'));

  test('static imports, live bindings, cycles and dynamic import()', () => {
    page.waitForText('#out', 'dyn:dyn.js', 2000);
    expect(page.text('#out')).toBe('5|0|1|dflt|add,counter,default,inc|Module|b:A|true|dyn:dyn.js|42');
  });

  test('module scripts run after classic scripts', () => {
    expect(page.text('body')).toContain('classic-first;');
  });
});

describe('Intl', () => {
  test('DateTimeFormat in a named time zone', () => {
    const r = page.evaluate(`(() => {
      const d = new Date(Date.UTC(2026, 9, 17, 19, 5, 9));
      const f = new Intl.DateTimeFormat("en-CA", { timeZone: "America/Chicago", year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
      const p = Object.fromEntries(f.formatToParts(d).map((x) => [x.type, x.value]));
      return [p.year, p.month, p.day, p.hour, p.minute].join(" ") + " | " + new Intl.DateTimeFormat("en-US", { timeZone: "UTC", dateStyle: "medium" }).format(d);
    })()`);
    expect(r).toBe('2026 10 17 14 05 | Oct 17, 2026');
  });

  test('invalid time zones throw RangeError', () => {
    expect(page.evaluate(`(() => { try { new Intl.DateTimeFormat("en", { timeZone: "Mars/Base" }); return "ok" } catch (e) { return e.name } })()`)).toBe('RangeError');
  });

  test('NumberFormat, PluralRules, Collator', () => {
    const r = page.evaluate(`[
      new Intl.NumberFormat("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(1234567.891),
      new Intl.NumberFormat("en", { notation: "compact" }).format(1234567),
      new Intl.PluralRules("en", { type: "ordinal" }).select(22),
      ["item 10", "Item 2", "item 1"].sort(new Intl.Collator(undefined, { numeric: true, sensitivity: "base" }).compare).join("|"),
    ].join(" ; ")`);
    expect(r).toBe('1,234,567.89 ; 1.2M ; two ; item 1|Item 2|item 10');
  });
});

describe('Proxy', () => {
  test('traps for get, has, set, ownKeys and apply', () => {
    const r = page.evaluate(`(() => {
      const seen = [];
      const p = new Proxy([3, 1, 2], { get(o, k, r) { if (typeof k === "string") seen.push(k); return Reflect.get(o, k, r); } });
      const ro = new Proxy({ a: 1 }, { set() { throw new Error("read-only"); } });
      let err = ""; try { ro.a = 2 } catch (e) { err = e.message }
      const f = new Proxy((x) => x + 1, { apply(t, th, args) { return t(...args) * 10 } });
      return [Array.isArray(p), p.map((x) => x * 2).join(), seen.includes("map"), err, f(1), typeof f, JSON.stringify({ ...ro })].join(" ");
    })()`);
    expect(r).toBe('true 6,2,4 true read-only 20 function {"a":1}');
  });
});

describe('lexer', () => {
  test('a slash after a statement\'s closing paren or brace starts a regex', () => {
    const r = page.evaluate(`(() => {
      const out = []; let e = "x";
      for (let i = 1; i--;) if (e.indexOf("x") >= 0) { e = "y"; break }/^a\\.\\w* b$/.test("a.c b") && out.push("block");
      if (true)/x/.test("x") && out.push("cond");
      const o = { a: 8 }.a / 2, h = (function () { return 6 })() / 3;
      out.push(o, h);
      return out.join(" ");
    })()`);
    expect(r).toBe('block cond 4 2');
  });
});
