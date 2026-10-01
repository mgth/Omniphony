// Runs in the page's own JavaScript world (manifest `"world": "MAIN"`) at
// document_start, before the player's scripts.
//
// 1. On youtube.com/tv, present the page as a TV: YouTube only offers IAMF
//    (itag 773) to its TV client. The request header is rewritten by the
//    extension's declarativeNetRequest rule; this covers what the page's
//    scripts read.
// 2. Watch Media Source Extensions: log every codec the player asks about
//    and every SourceBuffer it creates, and copy every byte appended to an
//    IAMF SourceBuffer. The browser keeps decoding and playing it as usual;
//    nothing is changed in what the player sees.
// 3. Expose `window.__omniphonyIamfCapture` for the popup: status, save
//    (downloads the capture as .mp4 and as a raw .iamf for orender), clear.

(() => {
  'use strict';

  const TV_USER_AGENT =
    'Mozilla/5.0 (Linux; Tizen 8.0) AppleWebKit/537.36 (KHTML, like Gecko) ' +
    '120.0.6099.5/8.0 TV Safari/537.36';
  const IAMF = /iamf/i;
  // Bound on what one capture holds, so a forgotten tab cannot eat the
  // machine's memory: about an hour of 7.1.4 Opus.
  const MAX_CAPTURE_BYTES = 512 * 1024 * 1024;

  if (location.pathname.startsWith('/tv')) {
    Object.defineProperty(Navigator.prototype, 'userAgent', {
      get: () => TV_USER_AGENT,
      configurable: true,
    });
    // The client hints would still say desktop Chrome.
    Object.defineProperty(Navigator.prototype, 'userAgentData', {
      get: () => undefined,
      configurable: true,
    });
  }

  const state = {
    // Codec questions the player asked, with the browser's answer.
    queries: new Map(),
    // Every SourceBuffer the player created: { mime, at }.
    sourceBuffers: [],
    // One capture per IAMF SourceBuffer, in creation order.
    captures: [],
  };

  const now = () => new Date().toISOString();

  // SourceBuffer → its capture, for the IAMF ones.
  const captureOf = new WeakMap();

  function videoId() {
    const fromHash = /[?&]v=([\w-]{6,})/.exec(location.hash);
    const fromSearch = new URLSearchParams(location.search).get('v');
    return (fromHash && fromHash[1]) || fromSearch || 'unknown';
  }

  const isTypeSupported = MediaSource.isTypeSupported.bind(MediaSource);
  MediaSource.isTypeSupported = function (type) {
    const answer = isTypeSupported(type);
    if (/^audio\//i.test(type) && !state.queries.has(type)) {
      state.queries.set(type, answer);
      if (IAMF.test(type)) console.info(`[omniphony] isTypeSupported(${type}) -> ${answer}`);
    }
    return answer;
  };

  const addSourceBuffer = MediaSource.prototype.addSourceBuffer;
  MediaSource.prototype.addSourceBuffer = function (mime) {
    const buffer = addSourceBuffer.call(this, mime);
    state.sourceBuffers.push({ mime, at: now() });
    if (IAMF.test(mime)) {
      const capture = {
        mime,
        videoId: videoId(),
        startedAt: now(),
        chunks: [],
        bytes: 0,
        appends: 0,
        truncated: false,
        firstTimestampOffset: buffer.timestampOffset,
      };
      state.captures.push(capture);
      captureOf.set(buffer, capture);
      console.info(`[omniphony] capturing IAMF SourceBuffer ${mime} (video ${capture.videoId})`);
    }
    return buffer;
  };

  const appendBuffer = SourceBuffer.prototype.appendBuffer;
  SourceBuffer.prototype.appendBuffer = function (data) {
    const capture = captureOf.get(this);
    if (capture) {
      const view = ArrayBuffer.isView(data)
        ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
        : new Uint8Array(data);
      capture.appends++;
      if (capture.bytes + view.byteLength <= MAX_CAPTURE_BYTES) {
        // A copy: the player may reuse its buffer after the call.
        capture.chunks.push(view.slice());
        capture.bytes += view.byteLength;
      } else {
        capture.truncated = true;
      }
    }
    return appendBuffer.call(this, data);
  };

  function concat(chunks, length) {
    const out = new Uint8Array(length);
    let at = 0;
    for (const c of chunks) {
      out.set(c, at);
      at += c.length;
    }
    return out;
  }

  function download(bytes, name, type) {
    const url = URL.createObjectURL(new Blob([bytes], { type }));
    const a = document.createElement('a');
    a.href = url;
    a.download = name;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 60_000);
  }

  window.__omniphonyIamfCapture = {
    status() {
      return {
        page: location.href,
        tvMode: location.pathname.startsWith('/tv'),
        userAgent: navigator.userAgent,
        iamfSupported: isTypeSupported('audio/mp4; codecs="iamf.001.001.Opus"'),
        queries: [...state.queries].map(([type, answer]) => ({ type, answer })),
        sourceBuffers: state.sourceBuffers.slice(-12),
        captures: state.captures.map((c, index) => ({
          index,
          mime: c.mime,
          videoId: c.videoId,
          startedAt: c.startedAt,
          bytes: c.bytes,
          appends: c.appends,
          truncated: c.truncated,
        })),
      };
    },

    // Download every non-empty capture: the bytes as appended (.mp4), and
    // the raw IAMF stream orender's bridge reads (.iamf).
    save() {
      const saved = [];
      state.captures.forEach((c, index) => {
        if (!c.bytes) return;
        const mp4 = concat(c.chunks, c.bytes);
        const base = `iamf-capture-${c.videoId}-${index}`;
        download(mp4, `${base}.mp4`, 'audio/mp4');
        const { iamf, sequences, samplesBytes } = globalThis.OmniphonyIamfMp4.toIamf(mp4);
        if (iamf.length) download(iamf, `${base}.iamf`, 'application/octet-stream');
        saved.push({ base, mp4Bytes: mp4.length, iamfBytes: iamf.length, sequences, samplesBytes });
      });
      return saved;
    },

    // Drop the captured data. The captures stay registered: their
    // SourceBuffers may still be appending.
    clear() {
      for (const c of state.captures) {
        c.chunks = [];
        c.bytes = 0;
        c.appends = 0;
        c.truncated = false;
      }
      return this.status();
    },
  };
})();
