describe('block layout', () => {
  test('empty inline elements make zero-height lines', () => {
    page.setContent('<body style="margin:0"><div id=d><span></span><h1 id=h style="margin:0">Title</h1><span></span> <span></span><p id=p style="margin:0">x</p></div></body>');
    expect(page.box('#h').y).toBe(0);
    expect(page.box('#p').y).toBe(page.box('#h').y + page.box('#h').height);
  });

  test('margins collapse through empty blocks', () => {
    page.setContent('<body style="margin:0"><div style="padding:1px"><h1 id=h style="margin:20px 0">T</h1><span></span><p id=p style="margin:20px 0">x</p></div></body>');
    const h = page.box('#h');
    expect(page.box('#p').y - (h.y + h.height)).toBe(20);
  });

  test('a legend sits on the fieldset border', () => {
    page.setContent('<body style="margin:0"><fieldset id=f style="margin:0;border:1px solid #999;padding:10px"><legend id=l style="padding:0;font-size:20px;line-height:20px">L</legend><div id=c>content</div></fieldset></body>');
    const f = page.box('#f'), l = page.box('#l'), c = page.box('#c');
    expect(l.y).toBe(f.y);
    // content starts below the legend plus padding (legend replaces the 1px top border)
    expect(c.y).toBe(f.y + 20 + 10);
  });

  test('a closed details shows only its summary', () => {
    page.setContent('<details><summary>S</summary><p id=hidden>body</p></details><details open><summary>T</summary><p id=shown>x</p></details>');
    expect(page.visible('#hidden')).toBe(false);
    expect(page.visible('#shown')).toBe(true);
  });

  test('logical inset properties position boxes', () => {
    page.setContent('<body style="margin:0"><div style="position:relative;width:300px;height:100px"><b id=b style="position:absolute;inset-inline-end:10px;inset-block-start:5px;width:20px;height:20px"></b></div></body>');
    expect(page.box('#b').x).toBe(270);
    expect(page.box('#b').y).toBe(5);
  });

  test('text-transform counts toward intrinsic width', () => {
    page.setContent('<div style="display:flex;gap:10px;font:12px sans-serif"><span id=a style="text-transform:uppercase">lot</span><span id=b>x</span></div>');
    const a = page.box('#a');
    expect(Math.round(page.box('#b').x - (a.x + a.width))).toBe(10);
  });
});
