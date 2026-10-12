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

  // MediaSource → the blob URL the player attached it through, to tell
  // whether the video element is playing a given capture (an ad plays its
  // own MediaSource in the same element).
  const urlOf = new WeakMap();
  const createObjectURL = URL.createObjectURL;
  URL.createObjectURL = function (object) {
    const url = createObjectURL.call(URL, object);
    if (object instanceof MediaSource) urlOf.set(object, url);
    return url;
  };

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
        mediaSource: this,
        demuxer: globalThis.OmniphonyIamfMp4.createDemuxer(),
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
      live.ingest(capture, capture.demuxer.push(view, this.timestampOffset));
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

  // ── Live: stream the IAMF track to orender ──────────────────────────
  //
  // Each temporal unit is sent just before the video reaches it: orender
  // plays what it receives as it receives it, so a unit sent `lead` seconds
  // ahead of its presentation time comes out of the speakers on time when
  // `lead` matches orender's latency. A (re)started orender needs about
  // 0.2 s before its first sample, which the lead also covers; the user's
  // offset is the rest (output latency). Pause and seek stop orender; play
  // restarts it at the playhead. The browser's own playback is muted.

  const STARTUP_S = 0.2;
  const TICK_MS = 20;
  // Units kept behind the playhead, for a short seek back.
  const KEEP_BEHIND_S = 5;

  const live = {
    enabled: false,
    offsetMs: 150,
    capture: null,
    units: [],
    config: null,
    configSent: false,
    cursor: null,
    orenderWanted: false,
    host: { state: 'disconnected' },
    sentUnits: 0,
    sentBytes: 0,
    timer: null,
    wasMuted: false,

    post(msg) {
      window.postMessage({ source: 'omniphony-page', msg }, location.origin);
    },

    ingest(capture, events) {
      if (capture !== this.capture) {
        // A new video (or the same one re-initialised): start over.
        this.capture = capture;
        this.units = [];
        this.config = null;
        this.cursor = null;
      }
      for (const e of events) {
        if (e.type === 'config') {
          this.config = e.obus;
          this.configSent = false;
          continue;
        }
        const units = this.units;
        if (!units.length || e.pts > units[units.length - 1].pts + 1e-6) {
          units.push(e);
          continue;
        }
        // A range appended again (after a seek back): replace or insert.
        const i = this.indexAfter(e.pts - 1e-6);
        if (i < units.length && Math.abs(units[i].pts - e.pts) < 1e-6) units[i] = e;
        else units.splice(i, 0, e);
      }
    },

    // First unit whose end is after `time`.
    indexAfter(time) {
      let lo = 0;
      let hi = this.units.length;
      while (lo < hi) {
        const mid = (lo + hi) >> 1;
        const u = this.units[mid];
        if (u.pts + u.duration <= time) lo = mid + 1;
        else hi = mid;
      }
      return lo;
    },

    video() {
      const video = document.querySelector('video');
      const c = this.capture;
      if (!video || !c) return null;
      const attached = video.srcObject === c.mediaSource || video.src === urlOf.get(c.mediaSource);
      return attached ? video : null;
    },

    stopOrender() {
      if (this.orenderWanted) this.post({ type: 'stop' });
      this.orenderWanted = false;
      this.cursor = null;
    },

    tick() {
      const video = this.video();
      const playing = video && !video.paused && !video.seeking && video.readyState >= 2;
      if (!playing) {
        this.stopOrender();
        return;
      }
      if (!video.muted) {
        this.wasMuted = false;
        video.muted = true;
      }
      if (!this.config) return;
      const lead = this.offsetMs / 1000 + STARTUP_S;
      const due = video.currentTime + lead;
      if (this.cursor === null) {
        // (Re)start at the playhead: what orender cannot play in time is
        // skipped.
        this.post({ type: 'start' });
        this.orenderWanted = true;
        this.configSent = false;
        this.cursor = due;
      }
      if (!this.configSent) {
        this.post({ type: 'config', data: base64(this.config) });
        this.configSent = true;
      }
      const parts = [];
      let bytes = 0;
      let i = this.indexAfter(this.cursor + 1e-6);
      while (i < this.units.length && this.units[i].pts < due) {
        const u = this.units[i++];
        parts.push(u.bytes);
        bytes += u.bytes.length;
        this.cursor = u.pts + u.duration;
      }
      if (parts.length) {
        this.post({ type: 'data', data: base64(concat(parts, bytes)) });
        this.sentUnits += parts.length;
        this.sentBytes += bytes;
      }
      const behind = this.indexAfter(video.currentTime - KEEP_BEHIND_S);
      if (behind > 0) this.units.splice(0, behind);
    },

    setEnabled(on) {
      if (on === this.enabled) return;
      this.enabled = on;
      const video = this.video() || document.querySelector('video');
      if (on) {
        this.wasMuted = video ? video.muted : false;
        this.timer = setInterval(() => this.tick(), TICK_MS);
      } else {
        clearInterval(this.timer);
        this.timer = null;
        this.stopOrender();
        if (video) video.muted = this.wasMuted;
      }
    },
  };

  window.addEventListener('message', (event) => {
    if (event.source !== window || event.data?.source !== 'omniphony-ext') return;
    const msg = event.data.msg;
    if (msg?.type === 'host') live.host = msg;
  });

  function base64(bytes) {
    let binary = '';
    for (let i = 0; i < bytes.length; i += 0x8000) {
      binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    }
    return btoa(binary);
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
        live: {
          enabled: live.enabled,
          offsetMs: live.offsetMs,
          host: live.host,
          sentUnits: live.sentUnits,
          sentBytes: live.sentBytes,
          // Seconds of IAMF the player has appended past the playhead.
          aheadS: (() => {
            const video = live.video();
            const last = live.units[live.units.length - 1];
            return video && last ? last.pts + last.duration - video.currentTime : 0;
          })(),
        },
      };
    },

    setLive(on, offsetMs) {
      if (Number.isFinite(offsetMs)) live.offsetMs = offsetMs;
      live.setEnabled(Boolean(on));
      return this.status();
    },

    // A running orender plays on from what it already holds, so a new offset
    // restarts it (a short gap) to take effect at once.
    setOffset(offsetMs) {
      if (!Number.isFinite(offsetMs) || offsetMs === live.offsetMs) return this.status();
      live.offsetMs = offsetMs;
      if (live.enabled) live.stopOrender();
      return this.status();
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
