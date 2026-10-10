// The static rule (rules.json) only rewrites the user agent of the
// youtube.com/tv page load itself. Once a tab is on the TV client, its other
// requests (the player API, the media) go out as a TV too, through a session
// rule scoped to that tab; ordinary youtube.com tabs are left alone.

const TV_USER_AGENT =
  'Mozilla/5.0 (Linux; Tizen 8.0) AppleWebKit/537.36 (KHTML, like Gecko) ' +
  '120.0.6099.5/8.0 TV Safari/537.36';
// Session rule ids: this base plus the tab id. The static rule uses id 1.
const RULE_BASE = 1000;

function tvRule(tabId) {
  return {
    id: RULE_BASE + tabId,
    priority: 1,
    action: {
      type: 'modifyHeaders',
      requestHeaders: [
        { header: 'User-Agent', operation: 'set', value: TV_USER_AGENT },
        { header: 'Sec-CH-UA', operation: 'remove' },
        { header: 'Sec-CH-UA-Mobile', operation: 'remove' },
        { header: 'Sec-CH-UA-Platform', operation: 'remove' },
      ],
    },
    condition: { tabIds: [tabId], requestDomains: ['youtube.com', 'googlevideo.com'] },
  };
}

async function setTvMode(tabId, on) {
  await chrome.declarativeNetRequest.updateSessionRules({
    removeRuleIds: [RULE_BASE + tabId],
    addRules: on ? [tvRule(tabId)] : [],
  });
}

chrome.tabs.onUpdated.addListener((tabId, change) => {
  if (change.url === undefined) return;
  setTvMode(tabId, change.url.startsWith('https://www.youtube.com/tv')).catch(console.error);
});

chrome.tabs.onRemoved.addListener((tabId) => {
  setTvMode(tabId, false).catch(console.error);
});

// Live streaming: each tab's relay (relay.js) gets its own native host, and
// so its own orender. Closing the tab disconnects the port, which closes the
// host's stdin, which stops orender.
const HOST = 'fr.mgth.omniphony.iamf';

chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== 'iamf-live') return;
  let native = null;
  try {
    native = chrome.runtime.connectNative(HOST);
  } catch (err) {
    port.postMessage({ type: 'host', state: 'error', error: String(err) });
    port.disconnect();
    return;
  }
  native.onMessage.addListener((msg) => port.postMessage(msg));
  native.onDisconnect.addListener(() => {
    const error = chrome.runtime.lastError?.message;
    port.postMessage({
      type: 'host',
      state: 'error',
      error: error || 'the native host exited',
      hint: error && /not found/i.test(error) ? 'run host/install-host.sh' : undefined,
    });
    port.disconnect();
  });
  port.onMessage.addListener((msg) => native.postMessage(msg));
  port.onDisconnect.addListener(() => native.disconnect());
});
