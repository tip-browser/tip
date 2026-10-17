describe('forms example', () => {
  beforeEach(() => page.goto('../examples/forms.html'));

  test('required field blocks submission', () => {
    page.click('button[type=submit]');
    expect(page.text('#out')).toContain('Fill the form');
  });

  test('serializes all control types with FormData', () => {
    page.type('#name', 'Ada');
    page.type('#email', 'ada@example.com');
    page.click('label[for=c2]');           // label forwards the click to the checkbox
    page.check('#r2');
    page.select('#plan', 'team');
    page.fill('#bio', 'Hello\nWorld');
    page.click('button[type=submit]');
    const out = JSON.parse(page.text('#out'));
    expect(out).toEqual({ name: 'Ada', email: 'ada@example.com', password: '', plan: 'team', topics: ['layout', 'js'], freq: 'monthly', size: '50', bio: 'Hello\nWorld' });
  });

  test('change events fire on select', () => {
    page.select('#plan', 'free');
    expect(page.console().map(c => c.text)).toContain('plan changed to free');
  });

  test('keyboard editing in text inputs', () => {
    page.type('#name', 'Adaa');
    page.press('Backspace');
    page.press('ArrowLeft');
    page.press('ArrowLeft');
    page.press('X');
    expect(page.value('#name')).toBe('AXda');
  });
});

describe('dropdowns and text selection', () => {
  beforeEach(() => page.goto('../examples/forms.html'));

  test('clicking a select opens a list instead of changing the value', () => {
    page.click('#plan');
    expect(page.dropdownOpen()).toBe(true);
    expect(page.value('#plan')).toBe('pro');
    // choose "Team" (third row) with the mouse: rows sit just below the select
    const b = page.box('#plan');
    const fs = 14, rh = Math.round(fs * 1.6);
    page.clickAt(b.x + 20, b.y + b.height + 2 + 4 + 2 * rh + rh / 2);
    expect(page.dropdownOpen()).toBe(false);
    expect(page.value('#plan')).toBe('team');
    expect(page.console().map(c => c.text)).toContain('plan changed to team');
  });

  test('keyboard navigation in an open dropdown', () => {
    page.click('#plan');
    page.press('ArrowUp');
    page.press('Enter');
    expect(page.value('#plan')).toBe('free');
    page.click('#plan');
    page.press('Escape');
    expect(page.dropdownOpen()).toBe(false);
    expect(page.value('#plan')).toBe('free');
  });

  test('shift+arrow selects text and typing replaces it', () => {
    page.type('#name', 'Hello World');
    for (let i = 0; i < 5; i++) page.press('Shift+ArrowLeft');
    expect(page.selection('#name')).toBe('World');
    page.type('#name', 'There');
    expect(page.value('#name')).toBe('Hello There');
  });

  test('select-all, word jumps and delete', () => {
    page.type('#name', 'one two three');
    page.press('Alt+ArrowLeft');
    page.press('Shift+Alt+ArrowLeft');
    expect(page.selection('#name')).toBe('two ');
    page.press('Backspace');
    expect(page.value('#name')).toBe('one three');
    page.press('Meta+a');
    expect(page.selection('#name')).toBe('one three');
    page.press('x');
    expect(page.value('#name')).toBe('x');
  });
});

describe('range sliders', () => {
  beforeEach(() => page.goto('../examples/forms.html'));

  test('renders and reports its default value', () => {
    expect(page.visible('#vol')).toBe(true);
    expect(page.box('#vol').height).toBeGreaterThan(10);
    expect(page.value('#vol')).toBe('50');
  });

  test('clicking the track sets the value (snapped to step) and fires events', () => {
    const b = page.box('#vol');
    page.clickAt(b.x + b.width * 0.81, b.y + b.height / 2);
    const v = Number(page.value('#vol'));
    expect(v % 5).toBe(0);
    expect(v).toBeGreaterThanOrEqual(75);
    expect(v).toBeLessThanOrEqual(85);
    expect(page.text('#vol-out')).toBe(String(v));
    expect(page.console().map(c => c.text)).toContain('size committed ' + v);
  });

  test('keyboard adjusts by step and clamps', () => {
    page.focus('#vol');
    page.press('ArrowRight');
    expect(page.value('#vol')).toBe('55');
    page.press('End');
    expect(page.value('#vol')).toBe('100');
    page.press('ArrowRight');
    expect(page.value('#vol')).toBe('100');
    page.press('Home');
    expect(page.text('#vol-out')).toBe('0');
  });

  test('scripts can read and write the value', () => {
    expect(page.evaluate(() => { const v = document.getElementById('vol'); v.value = 33; return [v.value, v.valueAsNumber]; })).toEqual(['35', 35]);
    expect(page.evaluate(() => { const v = document.getElementById('vol'); v.value = 500; return v.value; })).toBe('100');
  });
});

describe(':focus-visible', () => {
  const vis = (sel) => page.evaluate(`document.querySelector('${sel}').matches(':focus-visible')`);
  const foc = (sel) => page.evaluate(`document.querySelector('${sel}').matches(':focus')`);
  const setup = () => page.setContent('<style>:focus-visible { outline: 2px solid black }</style><button id=a>A</button><button id=b>B</button><input id=t>');

  test('a clicked button is focused but shows no ring', () => {
    setup();
    page.click('#a');
    expect(foc('#a')).toBe(true);
    expect(vis('#a')).toBe(false);
  });

  test('keyboard navigation shows the ring', () => {
    setup();
    page.click('#a');
    page.press('Tab');
    expect(foc('#b')).toBe(true);
    expect(vis('#b')).toBe(true);
  });

  test('text fields always show it', () => {
    setup();
    page.click('#t');
    expect(vis('#t')).toBe(true);
  });

  test('script focus follows the last interaction', () => {
    setup();
    page.click('#a');
    page.evaluate("document.getElementById('b').focus()");
    expect(vis('#b')).toBe(false);
  });
});
