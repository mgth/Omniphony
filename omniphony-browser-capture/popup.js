// The capture lives in the page (page-hook.js); the popup only asks it for
// its status and tells it to save or clear.

const $ = (id) => document.getElementById(id);

async function activeTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  return tab;
}

async function inPage(tab, method) {
  const [result] = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    world: 'MAIN',
    func: (name) => window.__omniphonyIamfCapture?.[name]() ?? null,
    args: [method],
  });
  return result?.result ?? null;
}

const kib = (n) => (n < 1 << 20 ? `${(n / 1024).toFixed(0)} KiB` : `${(n / (1 << 20)).toFixed(1)} MiB`);

function escape(text) {
  const span = document.createElement('span');
  span.textContent = text;
  return span.innerHTML;
}

function render(status) {
  const iamfQueries = status.queries.filter((q) => /iamf/i.test(q.type));
  const captures = status.captures.filter((c) => c.bytes > 0 || c.appends > 0);
  const flag = (on, yes, no) => `<span class="${on ? 'ok' : 'bad'}">${on ? yes : no}</span>`;
  $('content').innerHTML = `
    <dl>
      <dt>Client</dt><dd>${flag(status.tvMode, 'YouTube TV', 'desktop YouTube (no IAMF served)')}</dd>
      <dt>Browser IAMF</dt><dd>${flag(status.iamfSupported, 'decodes IAMF', 'cannot decode IAMF')}</dd>
      <dt>Player asked</dt><dd>${
        iamfQueries.length
          ? iamfQueries.map((q) => `${escape(q.type)} → ${q.answer}`).join('<br>')
          : '<span class="muted">no IAMF query yet</span>'
      }</dd>
    </dl>
    <div>Source buffers (latest):</div>
    <ul>${
      status.sourceBuffers.length
        ? status.sourceBuffers.map((b) => `<li>${escape(b.mime)}</li>`).join('')
        : '<li class="muted">none yet — start playback</li>'
    }</ul>
    <div>IAMF captures:</div>
    <ul>${
      captures.length
        ? captures
            .map(
              (c) =>
                `<li>#${c.index} ${escape(c.videoId)} — ${kib(c.bytes)} in ${c.appends} appends` +
                `${c.truncated ? ' <span class="bad">(full, truncated)</span>' : ''}</li>`,
            )
            .join('')
        : '<li class="muted">nothing captured</li>'
    }</ul>`;
  $('save').disabled = !captures.some((c) => c.bytes > 0);
  $('clear').disabled = !captures.length;
}

async function refresh() {
  const tab = await activeTab();
  const url = tab?.url ?? '';
  if (!url.startsWith('https://www.youtube.com/')) {
    $('content').textContent = 'Open a YouTube video first.';
    return;
  }
  const watch = new URL(url).searchParams.get('v');
  if (watch && !url.startsWith('https://www.youtube.com/tv')) {
    $('tv').hidden = false;
    $('tv').onclick = () =>
      chrome.tabs.update(tab.id, { url: `https://www.youtube.com/tv#/watch?v=${watch}` });
  }
  const status = await inPage(tab, 'status');
  if (!status) {
    $('content').textContent = 'The capture script is not in this page yet: reload it.';
    return;
  }
  render(status);
}

$('save').onclick = async () => {
  const saved = await inPage(await activeTab(), 'save');
  $('message').textContent = saved?.length
    ? saved
        .map((s) =>
          s.iamfBytes
            ? `${s.base}: ${kib(s.mp4Bytes)} .mp4, ${kib(s.iamfBytes)} .iamf`
            : `${s.base}: ${kib(s.mp4Bytes)} .mp4, no IAMF descriptors found`,
        )
        .join(' · ')
    : 'Nothing to save.';
};

$('clear').onclick = async () => {
  const status = await inPage(await activeTab(), 'clear');
  if (status) render(status);
  $('message').textContent = 'Cleared.';
};

refresh().catch((err) => {
  $('content').textContent = `Error: ${err.message}`;
});
setInterval(() => refresh().catch(() => {}), 1000);
