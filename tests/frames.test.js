// <iframe>: nested documents, postMessage and input routing.

describe('iframes', () => {
  test('frames load, render and talk to their parent', () => {
    page.goto('fixtures/frames/parent.html');
    page.waitForFunction('!!window.pong && window.srcdocBase !== undefined', 3000);
    expect(page.evaluate('loads')).toBe(1);
    expect(page.evaluate('readyFromKid')).toBe(true);
    expect(page.evaluate('kidSawTop')).toBe(true);
    expect(page.evaluate('JSON.stringify(pong)')).toBe(JSON.stringify({ kind: 'pong', n: 42, sameSource: true, origin: page.evaluate('location.origin') }));
    expect(page.evaluate('srcdocBase')).toBe(true);
    expect(page.evaluate('window.length')).toBe(2);
    expect(page.evaluate('window[0] === document.getElementById("f").contentWindow')).toBe(true);
    expect(page.evaluate('document.getElementById("f").contentDocument')).toBe(null);
    expect(page.evaluate('window.parent === window && window.top === window')).toBe(true);
  });

  test('clicks and typing reach the frame', () => {
    page.goto('fixtures/frames/parent.html');
    page.waitForFunction('!!window.pong', 3000);
    // the button sits inside the first iframe (at 0,0 400x200)
    page.clickAt(40, 75);
    page.waitForFunction('!!window.clicked', 2000);
    expect(page.evaluate('JSON.stringify(clicked.list)')).toBe('[1,2,{"deep":"yes"}]');
    page.clickAt(200, 72);
    for (const ch of 'hey') page.press(ch);
    page.waitForFunction('window.typed === "hey"', 2000);
    expect(page.evaluate('typed')).toBe('hey');
  });

  test('changing src navigates the frame; removing it drops the window', () => {
    page.goto('fixtures/frames/parent.html');
    page.waitForFunction('!!window.pong', 3000);
    page.evaluate('msgs.length = 0; document.getElementById("f").src = "child.html?again"');
    page.waitForFunction('msgs.includes("ready")', 3000);
    expect(page.evaluate('loads')).toBe(2);
    page.evaluate('document.getElementById("f").remove()');
    page.wait(50);
    expect(page.evaluate('window.length')).toBe(1);
  });

  test('MessagePorts can be transferred to a frame', () => {
    page.goto('fixtures/frames/parent.html');
    page.waitForFunction('!!window.pong', 3000);
    page.evaluate(`
      window.portMsgs = [];
      const ch = new MessageChannel();
      ch.port1.onmessage = (e) => {
        portMsgs.push(typeof e.data === 'string' ? e.data : JSON.stringify(e.data));
        if (portMsgs.length === 1) ch.port1.postMessage(21);
      };
      document.getElementById('f').contentWindow.postMessage({ kind: 'port' }, '*', [ch.port2]);
    `);
    page.waitForFunction('portMsgs.length === 2', 3000);
    expect(page.evaluate('portMsgs.join(" | ")')).toBe('hello over the port | {"doubled":42}');
  });

  test('target=_top links navigate the whole tab', () => {
    page.goto('fixtures/frames/parent.html');
    page.waitForFunction('!!window.pong', 3000);
    page.clickAt(30, 130);
    page.wait(200);
    expect(page.url().endsWith('parent.html?navigated')).toBe(true);
  });

  test('frames paint into the parent', () => {
    page.goto('fixtures/frames/parent.html');
    page.waitForFunction('!!window.pong', 3000);
    page.wait(50);
    // the child's light-blue background (#e0f2fe) shows inside the first iframe
    expect(page.pixel(390, 190)).toBe('rgb(224, 242, 254)');
    expect(page.pixel(390, 210)).not.toBe('rgb(224, 242, 254)');
  });
});
