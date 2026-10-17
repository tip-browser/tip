describe('image decoding', () => {
  beforeEach(() => page.goto('fixtures/modules/index.html'));

  const natural = (src) => {
    page.evaluate(`(() => { document.body.innerHTML = ''; const i = new Image(); i.id = 'im'; i.src = ${JSON.stringify(src)}; document.body.append(i); })()`);
    page.waitFor('#im', 1000);
    page.wait(300);
    return page.evaluate(`(() => { const i = document.getElementById('im'); return i.naturalWidth + 'x' + i.naturalHeight + ' ' + i.complete; })()`);
  };

  test('baseline JPEG', () => expect(natural('../images/baseline.jpg')).toBe('32x48 true'));
  test('progressive JPEG', () => expect(natural('../images/progressive.jpg')).toBe('32x48 true'));
  test('EXIF orientation is applied', () => expect(natural('../images/rotated.jpg')).toBe('48x32 true'));
  test('PNG', () => expect(natural('../images/pic.png')).toBe('32x48 true'));
  test('GIF', () => expect(natural('../images/pic.gif')).toBe('32x48 true'));

  test('load, error and decode()', () => {
    page.evaluate(`(() => {
      const log = [];
      const ok = new Image(); ok.onload = () => log.push('load ' + ok.naturalWidth);
      ok.src = '../images/baseline.jpg';
      const bad = new Image(); bad.onerror = () => log.push('error');
      bad.src = '../images/missing.png';
      const third = document.createElement('img');
      third.src = '../images/pic.gif';
      third.decode().then(() => { log.push('decoded'); document.title = log.sort().join(','); });
    })()`);
    page.wait(500);
    expect(page.title()).toBe('decoded,error,load 32');
  });
});
