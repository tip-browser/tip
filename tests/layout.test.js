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

describe('layout performance', () => {
  test('deeply nested flexboxes lay out in linear time', () => {
    let html = '<span id=leaf>leaf</span>';
    for (let i = 0; i < 24; i++) {
      html = `<div style="display:flex;flex-direction:column"><div style="display:flex"><div style="flex:1">${html}</div><div>x</div></div></div>`;
    }
    const t0 = Date.now();
    page.setContent(html);
    expect(page.box('#leaf').width > 0).toBe(true);
    expect(Date.now() - t0 < 2000).toBe(true);
  });
});

describe('flex sizing', () => {
  test('a column flex item never shrinks below its content (flex-basis 0)', () => {
    page.setViewport(1280, 713);
    page.setContent('<body style="margin:0"><div id=outer style="display:flex;flex-direction:column;min-height:100vh"><div style="display:flex;flex-direction:column;flex:1 1 0"><div style="display:flex;flex-grow:1"><div style="min-height:690px;flex:1"></div></div></div><div style="height:137px"></div></div></body>');
    expect(page.box('#outer').height).toBe(827);
    page.setViewport(1024, 768);
  });
});

describe('dialog sizing', () => {
  test('min() with mixed units resolves against the containing block', () => {
    page.setContent('<body style="margin:0"><div style="display:grid;place-items:center;width:1000px;height:400px"><div id=p style="width:min(460px, 100%);box-sizing:border-box">x</div></div><div style="width:300px"><div id=q style="width:min(460px, 100%)">y</div></div></body>');
    expect(page.box('#p').width).toBe(460);
    expect(page.box('#p').x).toBe(270);
    expect(page.box('#q').width).toBe(300);
  });

  test('max-content tracks get their content width next to fr tracks', () => {
    page.setContent('<dl id=d style="display:grid;grid-template-columns:minmax(0, max-content) minmax(0, 1fr);gap:0 16px;width:400px;margin:0;font:14px sans-serif"><dt id=a>Assembly and Test</dt><dd id=b style="margin:0">1.0</dd></dl>');
    const a = page.box('#a'), b = page.box('#b');
    expect(a.width > 50).toBe(true);
    expect(b.x).toBe(a.x + a.width + 16);
  });
});

describe('intrinsic sizing', () => {
  test('auto-fit columns with a min() minimum resolve against the grid width', () => {
    page.setContent(`<style>body{margin:0;width:800px} ul{list-style:none;margin:0;padding:0;display:grid;gap:28px 32px;grid-template-columns:repeat(auto-fit,minmax(min(100%,14rem),1fr))}</style>
      <ul><li id=a>one</li><li id=b>two</li><li id=c>three</li><li id=d>four</li></ul>`);
    expect(Math.round(page.box('#a').width)).toBe(245);
    expect(Math.round(page.box('#b').x)).toBe(277);
    expect(page.box('#d').x).toBe(0);
    expect(page.box('#d').y).toBeGreaterThan(page.box('#a').y);
  });

  test('calc() tracks resolve at layout time', () => {
    page.setContent(`<style>body{margin:0;width:400px} div{display:grid;grid-template-columns:calc(50% - 10px) 1fr}</style><div><i id=a>a</i><i id=b>b</i></div>`);
    expect(page.box('#a').width).toBe(190);
    expect(page.box('#b').x).toBe(190);
  });

  test('min-content allows breaks after hyphens', () => {
    page.setContent(`<style>body{margin:0} span{display:inline-block;width:min-content}</style><span id=s>Low-profile</span><span id=w>Lowprofile</span>`);
    expect(page.box('#s').width).toBeLessThan(page.box('#w').width);
  });

  test('width: fit-content / max-content / min-content inside grid and flex', () => {
    page.setContent(`<style>body{margin:0;width:600px} .g{display:grid} .f{display:flex;flex-direction:column} p{margin:0}</style>
      <div class=g><p id=a style="width:fit-content">tag</p><p id=b>stretch</p><p id=c style="width:max-content">a much longer line of text that is wide</p><p id=d style="width:min-content">two words</p></div>
      <div class=f><p id=e style="width:fit-content">in flex column</p></div><div><p id=f style="width:fit-content">block</p></div>`);
    expect(page.box('#a').width).toBeLessThan(60);
    expect(page.box('#b').width).toBe(600);
    expect(page.box('#c').width).toBeGreaterThan(200);
    expect(page.box('#c').width).toBeLessThan(600);
    expect(page.box('#d').width).toBeLessThan(page.box('#c').width / 3);
    expect(page.box('#e').width).toBeLessThan(150);
    expect(page.box('#f').width).toBeLessThan(80);
  });
});

describe('line breaking', () => {
  const long = 'https://www.example.com/search?q=' + 'abcdefghij'.repeat(12);
  test('line-break: anywhere and word-break: break-all wrap long words', () => {
    page.setContent(`<style>body{margin:0} div{width:200px;font-size:13px}</style>
      <div id=a style="line-break:anywhere">${long}</div>
      <div id=b style="word-break:break-all">${long}</div>
      <div id=c>${long}</div>`);
    for (const id of ['a', 'b']) {
      expect(page.evaluate(`document.getElementById('${id}').scrollWidth`)).toBeLessThan(201);
      expect(page.box('#' + id).height).toBeGreaterThan(40);
    }
    // without them the word stays on one (overflowing) line
    expect(page.box('#c').height).toBeLessThan(20);
  });

  test('overflow-wrap: break-word only breaks words that do not fit', () => {
    page.setContent(`<style>body{margin:0} div{width:200px;font-size:13px;overflow-wrap:break-word}</style>
      <div id=a>short words wrap normally</div><div id=b>${long}</div>
      <span id=s style="display:inline-block;overflow-wrap:break-word">${long}</span><span id=t style="display:inline-block;width:min-content;overflow-wrap:anywhere">${long}</span>`);
    expect(page.evaluate(`document.getElementById('b').scrollWidth`)).toBeLessThan(201);
    expect(page.box('#a').height).toBeLessThan(20);
    // break-word does not lower the min-content width; anywhere does
    expect(page.box('#t').width).toBeLessThan(20);
  });
});
