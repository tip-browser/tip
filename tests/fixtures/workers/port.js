// like reCAPTCHA's worker: hand the owner a port and serve requests on it
const ch = new MessageChannel();
ch.port1.onmessage = (e) => ch.port1.postMessage({ answer: e.data.q * 3, from: 'worker port' });
postMessage('hello-port', [ch.port2]);
