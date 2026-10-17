# Tip

A web browser built from scratch in Rust. It has **zero third-party crates**: the HTML parser, CSS engine, layout, rasterizer, font renderer, image decoder, HTTP client and JavaScript engine are all in this repository. The only outside code is what the operating system provides: windowing (Cocoa / Win32) and TLS for HTTPS.

```
tip                         # open the browser window (tip://home)
tip https://example.com     # open a URL
tip render page.html -o shot.png --full-page --scale 2
tip test tests/             # run JavaScript automation tests
tip dump layout page.html   # inspect the layout tree
tip eval page.html "document.title"
```

## Building

Requires a stable Rust toolchain (edition 2024).

```sh
cargo build --release          # target/release/tip (tip.exe on Windows)
cargo test --release           # engine unit tests
./target/release/tip test tests
./scripts/bundle-macos.sh      # dist/Tip.app
```

CI (`.github/workflows/ci.yml`) builds and tests on Linux, macOS and Windows, and publishes `tip-windows-x64.zip` (containing `tip.exe`) and `tip-macos.zip` (containing `Tip.app`) as build artifacts.

| Platform | Window | Headless (`render`, `test`, …) | HTTPS |
|---|---|---|---|
| macOS | ✓ (AppKit) | ✓ | ✓ (NSURLConnection) |
| Windows | ✓ (Win32) | ✓ | ✓ (WinHTTP) |
| Linux | — | ✓ | HTTP only |

## What's implemented

**HTML.** A forgiving tokenizer and tree builder: implied `html`/`head`/`body`, implicit end tags, raw-text elements and character references. `innerHTML` uses fragment parsing.

**CSS**
- Selectors: complex selectors, attribute selectors, `:not` / `:is` / `:where` / `:has`, `:nth-*`, `:hover` / `:focus`, and `::before` / `::after`.
- Values: custom properties with `var()`, `calc()` / `min()` / `max()` / `clamp()`, and `@media`.
- Cascade: shorthands and inheritance.

**Layout**
- Block flow with margin collapsing and floats.
- Inline formatting: line breaking, whitespace modes, `text-align`, `vertical-align`, and inline box decorations.
- Flexbox: grow/shrink with min/max freezing, wrapping, alignment, auto margins and `order`.
- Grid: `fr` units, `repeat()`, `minmax()`, `auto-fill`, spans and auto-placement.
- Tables: auto column widths, `colspan`, collapsed borders.
- Positioning: absolute, fixed and relative, plus `transform: translate`.
- Scroll containers and form controls.

**Painting.** Anti-aliased coverage rasterizer, rounded corners, borders (solid/dashed/dotted/double), linear and radial gradients, blurred box shadows, opacity, z-index stacking and clipping. Output is HiDPI-aware.

**Text.** A built-in TrueType parser and rasterizer (composite glyphs, kerning, synthetic bold/italic) using system fonts, with fallback fonts for other scripts.

**Images.** Our own DEFLATE and PNG decoder (all colour types, interlacing). Images load on background threads.

**Networking.** An HTTP/1.1 client with chunked encoding, gzip/deflate, redirects and cookies. HTTPS goes through the OS. Also `file:`, `data:` and `about:` URLs, and the internal `tip:` scheme.

**JavaScript.** A tree-walking interpreter supporting:
- `let`/`const`, closures, destructuring, spread and rest
- classes with private fields, getters/setters and `super`
- template literals, optional chaining, `??`, BigInt
- generators, `async`/`await` and `for await`, labelled statements
- its own regex engine (lookbehind, named groups, flags)
- the standard library: Object, Array, String, Number, Math, JSON, Date, RegExp, Promise, Map/Set/WeakMap, Symbol, typed arrays, Reflect

**DOM and Web APIs**
- Nodes and elements: traversal and mutation, `classList`, `dataset`, `style`, `getBoundingClientRect` / offset properties, `getComputedStyle`.
- Events: capture and bubble, `preventDefault`, inline `on*` handlers, and constructors for the event types.
- Timers and `requestAnimationFrame`.
- Networking: `fetch` and `XMLHttpRequest`, both non-blocking.
- Storage: `localStorage` (persisted to `~/.tip/`) and `sessionStorage`.
- `URL` / `URLSearchParams`, `FormData`, `history` / `location`.
- Observers: `MutationObserver`, `IntersectionObserver`, `ResizeObserver`.
- `DOMParser`, `TextEncoder`, `crypto.randomUUID`, and more.

**Browser.** Tabs, back/forward history, address bar, zoom, keyboard shortcuts, form submission (GET/POST) and text editing with a caret. The toolbar and DevTools are HTML pages rendered by the engine itself.

**DevTools** (F12 / ⌘⌥I)
- Elements: DOM tree, element picker, box model, matched CSS rules with overridden declarations, computed styles.
- Console: page logs and errors, plus a JS REPL.
- Network: every request with status, type, size and timing.
- Application: page info and cookies.

## Test automation

Test files (`*.test.js`) run headlessly against a real page. The `page` object drives the page through the same input pipeline as the window (hit-testing, focus, key events, default actions):

```js
describe('todo app', () => {
  beforeEach(() => page.goto('../examples/todo.html'));   // relative to the test file

  test('adds an item', () => {
    page.type('#new', 'Write tests');
    page.press('#new', 'Enter');
    expect(page.count('#list li')).toBe(4);
    expect(page.text('#count')).toBe('3 items left');
    page.screenshot('shots/after-add.png');
  });
});
```

- **Navigation:** `goto(url)`, `setContent(html)`, `back()`, `forward()`, `reload()`, `url()`, `title()`
- **Input:** `click(sel)`, `dblclick(sel)`, `hover(sel)`, `focus(sel)`, `type(sel, text)`, `fill(sel, text)`, `press([sel,] key)`. Keys accept modifiers, e.g. `"Control+a"`.
- **Form controls:** `check(sel)`, `uncheck(sel)`, `select(sel, value)`
- **Queries:** `text`, `html`, `attr`, `value`, `isChecked`, `exists`, `count`, `visible`, `box`
- **Scripts:** `evaluate(fn | code, ...args)` runs in the page; promises are awaited and results come back via JSON.
- **Waiting:** `waitFor(sel, ms)`, `waitForText(sel, text, ms)`, `wait(ms)`
- **Output:** `screenshot(path | {path, fullPage})`, `console()`, `alerts()`
- **Viewport:** `setViewport(w, h)`, `scroll(x, y)`

`test`/`it`, `describe`, `beforeEach` and `afterEach` structure the tests. `expect` provides `toBe`, `toEqual`, `toContain`, `toMatch`, `toBeTruthy`, `toBeGreaterThan`, `toHaveLength`, `toHaveProperty`, `toThrow` and `.not`. Use `--junit report.xml` for CI and `--verbose` to print page console output.

## Architecture

```
src/
  dom.rs html.rs            DOM arena, HTML tokenizer + tree builder
  css.rs style.rs ua.css    CSS parsing, selector matching, cascade → ComputedStyle
  layout/                   box tree, block/inline/flex/grid/table/positioned layout
  paint.rs raster.rs        display list (stacking, clips) → software rasterizer
  text/                     TrueType parsing, glyph rasterization, shaping-free text measurement
  image/                    inflate (+zlib/gzip), PNG decode/encode
  net/                      URL parsing, HTTP/1.1 client, OS TLS bridge, cookies
  js/                       lexer, parser, interpreter, builtins, regex, coroutines
  bindings/                 DOM host objects, events, window APIs
  page.rs input.rs tab.rs   documents, input handling, navigation/history
  app.rs devtools.rs        browser UI (rendered by the engine) and DevTools
  platform/                 macOS (Objective-C runtime) and Windows (Win32) backends
  automation.rs             `tip test` runner
```

How the design fits together:

- **Generators and `async` functions.** These need to suspend mid-function, which a tree-walking interpreter can't do on its own. Each one runs on its own OS thread. Control passes between threads over channels, so exactly one thread runs JavaScript at a time.
- **Recursion limit.** Deep JS recursion is bounded by measuring how much native stack is actually in use, rather than by a fixed call depth. `build.rs` gives the main thread a 256 MB stack.
- **Page loads.** Documents are fetched on a background thread, which also prefetches their stylesheets and scripts in parallel. The page is then built on the main thread.

## Known limitations

These are areas the engine doesn't cover yet:

- right-to-left and complex-script shaping (e.g. Arabic)
- colour emoji
- JPEG, GIF and WebP (shown as alt text)
- `<canvas>`, SVG rendering, video
- CSS transforms other than translate, animations and transitions
- ES modules, Shadow DOM, Web Workers
- IME input
- Windows: the release `tip.exe` is a GUI-subsystem binary, so console output from CLI commands attaches to the parent console.
