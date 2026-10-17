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

describe('replaced and table sizing', () => {
  test('an svg with only a viewBox fills the available width', () => {
    page.setContent('<a style="display:inline-block;box-sizing:border-box;width:40px;height:40px;padding:8px"><svg id=s viewBox="0 0 24 24"><path d="M0 0h24v24z"/></svg></a>');
    expect(page.box('#s').width).toBe(24);
    expect(page.box('#s').height).toBe(24);
  });

  test('percentage columns widen an auto-width table', () => {
    page.setContent('<body style="margin:0;width:1280px"><table id=t cellpadding=0 cellspacing=0 style="margin:auto"><tr><td width="25%">&nbsp;</td><td id=m><div style="width:514px;height:10px"></div></td><td width="25%">x</td></tr></table></body>');
    expect(page.box('#t').width).toBe(1028);
    expect(page.box('#m').x).toBe(383);
  });

  test('valign on a row aligns its cells', () => {
    page.setContent('<table cellpadding=0 cellspacing=0><tr valign=top><td><div style="height:60px"></div></td><td><div id=a>top</div></td></tr></table>');
    expect(page.box('#a').y).toBe(page.box('table').y);
  });

  test('buttons use border-box sizing', () => {
    page.setContent('<input id=b type=submit value=Go style="height:30px">');
    expect(page.box('#b').height).toBe(30);
  });
});
