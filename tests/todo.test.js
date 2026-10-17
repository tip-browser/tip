// End-to-end tests for examples/todo.html, driven through real input events.

describe('todo app', () => {
  beforeEach(() => page.goto('../examples/todo.html'));

  test('renders the initial items', () => {
    expect(page.title()).toBe('(2) Tip Todo');
    expect(page.count('#list li')).toBe(3);
    expect(page.text('#count')).toBe('2 items left');
    expect(page.attr('#list li:first-child', 'class')).toBe('done');
  });

  test('adds an item by typing and pressing Enter', () => {
    page.type('#new', 'Write tests');
    page.press('#new', 'Enter');
    expect(page.count('#list li')).toBe(4);
    expect(page.text('#list li:last-child .label')).toBe('Write tests');
    expect(page.value('#new')).toBe('');
    expect(page.text('#count')).toBe('3 items left');
  });

  test('adds an item with the button', () => {
    page.fill('#new', 'Via button');
    page.click('#add-btn');
    expect(page.text('#list li:last-child .label')).toBe('Via button');
  });

  test('toggles completion with the checkbox', () => {
    page.click('#list li:nth-child(2) input');
    expect(page.isChecked('#list li:nth-child(2) input')).toBe(true);
    expect(page.text('#count')).toBe('1 item left');
  });

  test('deletes and clears', () => {
    page.click('#list li:nth-child(3) .del');
    expect(page.count('#list li')).toBe(2);
    page.click('#clear');
    expect(page.count('#list li')).toBe(1);
    expect(page.evaluate(() => document.querySelectorAll('#list li').length)).toBe(1);
  });

  test('takes a screenshot', () => {
    const path = page.screenshot('../target/screenshots/todo.png');
    expect(path).toContain('todo.png');
  });
});
