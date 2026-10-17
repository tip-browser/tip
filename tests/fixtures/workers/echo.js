importScripts('helper.js');
self.onmessage = (e) => {
  if (e.data === 'env') {
    postMessage({ doc: typeof document, win: typeof window, selfOk: self === globalThis, scope: self instanceof WorkerGlobalScope, name: self.name, helper: helperValue });
    return;
  }
  if (e.data === 'later') { setTimeout(() => postMessage('timer fired'), 10); return; }
  if (e.data === 'bye') { postMessage('closing'); close(); return; }
  postMessage({ echo: e.data, doubled: e.data.n * 2 });
};
postMessage('ready');
