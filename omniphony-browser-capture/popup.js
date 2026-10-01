// The capture lives in the page (page-hook.js); the popup only asks it for
// its status and tells it to save or clear.

const $ = (id) => document.getElementById(id);

async function activeTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  return tab;
}

async function inPage(tab, method, ...args) {
  const [result] = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    world: 'MAIN',
    func: (name, rest) => window.__omniphonyIamfCapture?.[name](...rest) ?? null,
    args: [method, args],
  });
  return result?.result ?? null;
}

// The audio offset is the user's calibration of orender's output latency:
// kept across pages and sessions.
async function storedOffset() {
  const { offsetMs } = await chrome.storage.local.get({ offsetMs: 150 });
  return offsetMs;
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
  renderLive(status.live);
}

function renderLive(live) {
  const host = live.host || {};
  const hostText =
    host.state === 'running'
      ? `<span class="ok">orender running</span> (pid ${host.pid}, ${kib(host.bytes || 0)} written)`
      : host.state === 'error'
        ? `<span class="bad">${escape(host.error || 'error')}</span>${host.hint ? ` — ${escape(host.hint)}` : ''}`
        : `<span class="muted">orender ${escape(host.state || 'not started')}</span>`;
  $('live').innerHTML = live.enabled
    ? `${hostText}<br>sent ${live.sentUnits} units (${kib(live.sentBytes)}), ` +
      `${live.aheadS.toFixed(1)} s appended ahead`
    : 'Off: the browser plays the audio.';
  $('liveToggle').disabled = false;
  $('liveToggle').textContent = live.enabled ? 'Stop live' : 'Start live';
  $('liveToggle').dataset.on = live.enabled ? '1' : '';
  $('offset').textContent = `${live.offsetMs} ms`;
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

$('liveToggle').onclick = async () => {
  const on = !$('liveToggle').dataset.on;
  const status = await inPage(await activeTab(), 'setLive', on, await storedOffset());
  if (status) render(status);
  $('message').textContent = on
    ? 'Live: the browser is muted and orender plays the IAMF track.'
    : 'Live stopped.';
};

async function nudgeOffset(deltaMs) {
  const offsetMs = (await storedOffset()) + deltaMs;
  await chrome.storage.local.set({ offsetMs });
  const status = await inPage(await activeTab(), 'setOffset', offsetMs);
  if (status) render(status);
}
$('offsetDown').onclick = () => nudgeOffset(-25);
$('offsetUp').onclick = () => nudgeOffset(25);

$('clear').onclick = async () => {
  const status = await inPage(await activeTab(), 'clear');
  if (status) render(status);
  $('message').textContent = 'Cleared.';
};

refresh().catch((err) => {
  $('content').textContent = `Error: ${err.message}`;
});
setInterval(() => refresh().catch(() => {}), 1000);
