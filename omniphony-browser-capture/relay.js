// Isolated-world content script: the page script (page-hook.js, MAIN world)
// has no extension APIs, so its live-streaming messages come through here to
// the service worker, which holds the native messaging port, and the host's
// answers go back the same way.

let port = null;

function connect() {
  port = chrome.runtime.connect({ name: 'iamf-live' });
  port.onMessage.addListener((msg) => {
    window.postMessage({ source: 'omniphony-ext', msg }, location.origin);
  });
  port.onDisconnect.addListener(() => {
    port = null;
    window.postMessage(
      { source: 'omniphony-ext', msg: { type: 'host', state: 'disconnected' } },
      location.origin,
    );
  });
}

window.addEventListener('message', (event) => {
  if (event.source !== window || event.data?.source !== 'omniphony-page') return;
  const msg = event.data.msg;
  // Only the host protocol's message types are relayed.
  if (!['start', 'config', 'data', 'flush', 'stop', 'status'].includes(msg?.type)) return;
  if (!port) connect();
  port.postMessage(msg);
});
