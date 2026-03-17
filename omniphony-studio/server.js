const path = require('path');
const http = require('http');
const express = require('express');
const WebSocket = require('ws');
const osc = require('osc');
const { parseOscMessage } = require('./src/oscParser');
const { loadLayouts } = require('./src/layouts');

function parseCliArgs(argv) {
  const out = {};

  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (!token.startsWith('--')) {
      continue;
    }

    const [rawKey, inlineValue] = token.slice(2).split('=');
    const key = rawKey.trim();
    if (!key) {
      continue;
    }

    if (inlineValue !== undefined) {
      out[key] = inlineValue;
      continue;
    }

    const next = argv[i + 1];
    if (next && !next.startsWith('--')) {
      out[key] = next;
      i += 1;
      continue;
    }

    out[key] = true;
  }

  return out;
}

function toPort(value, fallback) {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed > 0 && parsed <= 65535 ? parsed : fallback;
}

function toListenPort(value, fallback) {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed >= 0 && parsed <= 65535 ? parsed : fallback;
}

const args = parseCliArgs(process.argv.slice(2));

const HTTP_PORT = toPort(args.httpPort ?? args['http-port'] ?? process.env.PORT, 3000);
const OSC_PORT = toListenPort(args.oscPort ?? args['osc-port'] ?? process.env.OSC_PORT, 0);
const OSC_HOST = String(args.host ?? args.oscHost ?? args['osc-host'] ?? process.env.OSC_HOST ?? '127.0.0.1');
const OSC_RX_PORT = toPort(args.oscRxPort ?? args['osc-rx-port'] ?? process.env.OSC_RX_PORT, 9000);
const HEARTBEAT_INTERVAL_MS = 5000;
const HEARTBEAT_ACK_TIMEOUT_MS = 10000;


const app = express();
app.use(express.static(path.join(__dirname, 'public')));

const server = http.createServer(app);
const wss = new WebSocket.Server({ server });

const layouts = loadLayouts();

// Latency smoothing: EMA with α=0.03 → τ≈1.7 s at 20 Hz metering rate.
// Absorbs mpv burst-fill oscillations without hiding real latency drift.
const LATENCY_EMA_ALPHA = 0.03;
let latencyEma = null;

const state = {
  sources: {},
  sourceLevels: {},
  speakerLevels: {},
  objectSpeakerGains: {},
  objectGains: {},
  speakerGains: {},
  objectMutes: {},
  speakerMutes: {},
  roomRatio: { width: 1, length: 2, height: 1 },
  spread: { min: null, max: null },
  loudness: null,
  loudnessSource: null,
  loudnessGain: null,
  masterGain: null,
  distanceDiffuse: { enabled: null, threshold: null, curve: null },
  configSaved: null,
  latencyMs: null,
  resampleRatio: null,
  layouts,
  selectedLayoutKey: layouts[0]?.key || null
};
const oscParseContext = { omniphonyCoordinateFormat: 0 };

function broadcast(payload) {
  const message = JSON.stringify(payload);
  wss.clients.forEach((client) => {
    if (client.readyState === WebSocket.OPEN) {
      client.send(message);
    }
  });
}

function buildLiveLayoutFromSpeakers(speakers) {
  return {
    key: 'gsrd-live',
    name: 'omniphony (live)',
    speakers: speakers.map((speaker) => ({
      id: speaker.name || `spk-${speaker.index}`,
      x: speaker.position.x,
      y: speaker.position.y,
      z: speaker.position.z,
      spatialize: speaker.spatialize
    }))
  };
}

function applyOmniphonySpeakerConfig(speakers) {
  if (!Array.isArray(speakers) || speakers.length === 0) {
    return;
  }

  const liveLayout = buildLiveLayoutFromSpeakers(speakers);
  const withoutLive = state.layouts.filter((layout) => layout.key !== liveLayout.key);
  state.layouts = [liveLayout, ...withoutLive];
  state.selectedLayoutKey = liveLayout.key;

  broadcast({
    type: 'layouts:update',
    layouts: state.layouts,
    selectedLayoutKey: state.selectedLayoutKey
  });
}

function handleParsedOsc(parsed) {
  if (!parsed) {
    return;
  }

  if (parsed.type === 'update') {
    state.sources[parsed.id] = {
      ...parsed.position,
      ...(parsed.name ? { name: parsed.name } : {}),
      updatedAt: Date.now()
    };

    broadcast({
      type: 'source:update',
      id: parsed.id,
      position: state.sources[parsed.id]
    });
  }

  if (parsed.type === 'remove') {
    delete state.sources[parsed.id];
    delete state.sourceLevels[parsed.id];
    delete state.objectSpeakerGains[parsed.id];
    broadcast({ type: 'source:remove', id: parsed.id });
  }

  if (parsed.type === 'meter:object') {
    state.sourceLevels[parsed.id] = {
      peakDbfs: parsed.peakDbfs,
      rmsDbfs: parsed.rmsDbfs,
      updatedAt: Date.now()
    };

    broadcast({
      type: 'source:meter',
      id: parsed.id,
      meter: state.sourceLevels[parsed.id]
    });
  }


  if (parsed.type === 'meter:object:gains') {
    state.objectSpeakerGains[parsed.id] = {
      gains: parsed.gains,
      updatedAt: Date.now()
    };

    broadcast({
      type: 'source:gains',
      id: parsed.id,
      gains: parsed.gains
    });
  }

  if (parsed.type === 'meter:speaker') {
    state.speakerLevels[parsed.id] = {
      peakDbfs: parsed.peakDbfs,
      rmsDbfs: parsed.rmsDbfs,
      updatedAt: Date.now()
    };

    broadcast({
      type: 'speaker:meter',
      id: parsed.id,
      meter: state.speakerLevels[parsed.id]
    });
  }

  if (parsed.type === 'state:object:gain') {
    state.objectGains[parsed.id] = parsed.gain;
    broadcast({
      type: 'object:gain',
      id: parsed.id,
      gain: parsed.gain
    });
  }

  if (parsed.type === 'state:speaker:gain') {
    state.speakerGains[parsed.id] = parsed.gain;
    broadcast({
      type: 'speaker:gain',
      id: parsed.id,
      gain: parsed.gain
    });
  }

  if (parsed.type === 'state:object:mute') {
    if (parsed.muted) {
      state.objectMutes[parsed.id] = 1;
    } else {
      delete state.objectMutes[parsed.id];
    }
    broadcast({
      type: 'object:mute',
      id: parsed.id,
      muted: parsed.muted ? 1 : 0
    });
  }

  if (parsed.type === 'state:speaker:mute') {
    if (parsed.muted) {
      state.speakerMutes[parsed.id] = 1;
    } else {
      delete state.speakerMutes[parsed.id];
    }
    broadcast({
      type: 'speaker:mute',
      id: parsed.id,
      muted: parsed.muted ? 1 : 0
    });
  }

  if (parsed.type === 'state:room_ratio') {
    state.roomRatio = {
      width: parsed.width,
      length: parsed.length,
      height: parsed.height
    };
    broadcast({
      type: 'room_ratio',
      roomRatio: state.roomRatio
    });
  }

  if (parsed.type === 'state:spread:min') {
    state.spread.min = parsed.value;
    broadcast({
      type: 'spread:min',
      value: parsed.value
    });
  }

  if (parsed.type === 'state:spread:max') {
    state.spread.max = parsed.value;
    broadcast({
      type: 'spread:max',
      value: parsed.value
    });
  }

  if (parsed.type === 'state:loudness') {
    state.loudness = parsed.enabled ? 1 : 0;
    broadcast({
      type: 'loudness',
      enabled: state.loudness
    });
  }

  if (parsed.type === 'state:loudness:source') {
    state.loudnessSource = parsed.value;
    broadcast({
      type: 'loudness:source',
      value: parsed.value
    });
  }

  if (parsed.type === 'state:loudness:gain') {
    state.loudnessGain = parsed.value;
    broadcast({
      type: 'loudness:gain',
      value: parsed.value
    });
  }

  if (parsed.type === 'state:master:gain') {
    state.masterGain = parsed.value;
    broadcast({
      type: 'master:gain',
      value: parsed.value
    });
  }

  if (parsed.type === 'state:distance_diffuse:enabled') {
    state.distanceDiffuse.enabled = parsed.enabled;
    broadcast({ type: 'distance_diffuse:enabled', enabled: parsed.enabled });
  }

  if (parsed.type === 'state:distance_diffuse:threshold') {
    state.distanceDiffuse.threshold = parsed.value;
    broadcast({ type: 'distance_diffuse:threshold', value: parsed.value });
  }

  if (parsed.type === 'state:distance_diffuse:curve') {
    state.distanceDiffuse.curve = parsed.value;
    broadcast({ type: 'distance_diffuse:curve', value: parsed.value });
  }

  if (parsed.type === 'state:config:saved') {
    state.configSaved = parsed.saved ? 1 : 0;
    broadcast({ type: 'config:saved', saved: state.configSaved });
  }

  if (parsed.type === 'state:latency') {
    latencyEma = latencyEma === null
      ? parsed.value
      : LATENCY_EMA_ALPHA * parsed.value + (1 - LATENCY_EMA_ALPHA) * latencyEma;
    state.latencyMs = Math.round(latencyEma);
    broadcast({
      type: 'latency',
      value: state.latencyMs
    });
  }

  if (parsed.type === 'state:resample_ratio') {
    state.resampleRatio = parsed.value;
    broadcast({
      type: 'resample_ratio',
      value: parsed.value
    });
  }
}

function handleOscMessage(oscMsg) {
  if (handleHeartbeatResponseAddress(oscMsg?.address)) {
    return;
  }

  handleParsedOsc(parseOscMessage(oscMsg, oscParseContext));
}

function handleOscBundle(bundle) {
  const packets = Array.isArray(bundle?.packets) ? bundle.packets : [];
  const configPackets = [];

  packets.forEach((packet) => {
    if (!packet?.address) {
      return;
    }

    if (handleHeartbeatResponseAddress(packet.address)) {
      return;
    }

    const parsed = parseOscMessage(packet, oscParseContext);
    if (!parsed) {
      return;
    }

    if (parsed.type.startsWith('config:')) {
      configPackets.push(parsed);
      return;
    }

    handleParsedOsc(parsed);
  });

  if (configPackets.length === 0) {
    return;
  }

  const countMessage = configPackets.find((packet) => packet.type === 'config:speakers:count');
  const speakerPackets = configPackets
    .filter((packet) => packet.type === 'config:speaker')
    .sort((a, b) => a.index - b.index);

  const count = countMessage?.count;
  if (typeof count === 'number' && speakerPackets.length !== count) {
    console.warn(`[osc] omniphony speaker config count mismatch: expected ${count}, got ${speakerPackets.length}`);
  }

  applyOmniphonySpeakerConfig(speakerPackets);
}

const oscUdpPort = new osc.UDPPort({
  localAddress: '0.0.0.0',
  localPort: OSC_PORT,
  metadata: true
});

let heartbeatInterval = null;
let activeListenPort = null;
let lastHeartbeatAckAt = 0;

function sendOmniphonyControlMessage(address, listenPort) {
  oscUdpPort.send(
    {
      address,
      args: [{ type: 'i', value: listenPort }]
    },
    OSC_HOST,
    OSC_RX_PORT
  );
}

function sendOmniphonyFloatControl(address, value) {
  oscUdpPort.send(
    {
      address,
      args: [{ type: 'f', value }]
    },
    OSC_HOST,
    OSC_RX_PORT
  );
}

function sendOmniphonyIntControl(address, value) {
  oscUdpPort.send(
    {
      address,
      args: [{ type: 'i', value }]
    },
    OSC_HOST,
    OSC_RX_PORT
  );
}

function sendOmniphonyNoArgs(address) {
  oscUdpPort.send(
    {
      address
    },
    OSC_HOST,
    OSC_RX_PORT
  );
}

function registerToOmniphony(listenPort, reason = 'startup') {
  activeListenPort = listenPort;
  latencyEma = null;
  sendOmniphonyControlMessage('/omniphony/register', listenPort);
  lastHeartbeatAckAt = Date.now();
  console.log(`[osc] register sent to udp://${OSC_HOST}:${OSC_RX_PORT} with listen_port=${listenPort} (${reason})`);
}

function handleHeartbeatResponseAddress(address) {
  const normalized = String(address || '').toLowerCase();
  if (normalized === '/omniphony/heartbeat/ack') {
    lastHeartbeatAckAt = Date.now();
    return true;
  }

  if (normalized === '/omniphony/heartbeat/unknown') {
    if (activeListenPort !== null) {
      registerToOmniphony(activeListenPort, 'heartbeat unknown');
    }
    return true;
  }

  return false;
}

function stopHeartbeat() {
  if (heartbeatInterval) {
    clearInterval(heartbeatInterval);
    heartbeatInterval = null;
  }
}

function startHeartbeat(listenPort) {
  stopHeartbeat();
  activeListenPort = listenPort;
  lastHeartbeatAckAt = Date.now();

  heartbeatInterval = setInterval(() => {
    sendOmniphonyControlMessage('/omniphony/heartbeat', listenPort);

    const ackAgeMs = Date.now() - lastHeartbeatAckAt;
    if (ackAgeMs > HEARTBEAT_ACK_TIMEOUT_MS) {
      registerToOmniphony(listenPort, `heartbeat timeout ${Math.round(ackAgeMs)}ms`);
    }
  }, HEARTBEAT_INTERVAL_MS);

  if (typeof heartbeatInterval.unref === 'function') {
    heartbeatInterval.unref();
  }
}

oscUdpPort.on('ready', () => {
  const listenPort = oscUdpPort.socket?.address?.().port || OSC_PORT;
  console.log(`[osc] listening on udp://0.0.0.0:${listenPort}`);

  registerToOmniphony(listenPort);

  startHeartbeat(listenPort);
  console.log(`[osc] heartbeat started: /omniphony/heartbeat every ${HEARTBEAT_INTERVAL_MS / 1000}s`);
});

oscUdpPort.on('message', handleOscMessage);
oscUdpPort.on('bundle', handleOscBundle);

oscUdpPort.on('error', (err) => {
  console.error('[osc] error:', err.message);
});

wss.on('connection', (ws) => {
  ws.on('message', (message) => {
    try {
      const payload = JSON.parse(message.toString());
      if (payload?.type === 'layout:select') {
        const hasLayout = state.layouts.some((layout) => layout.key === payload.key);
        if (!hasLayout) {
          return;
        }
        state.selectedLayoutKey = payload.key;
        broadcast({ type: 'layout:selected', key: state.selectedLayoutKey });
      }

      if (payload?.type === 'control:object:gain') {
        const id = Number(payload.id);
        const gain = Number(payload.gain);
        if (!Number.isFinite(id) || id < 0 || !Number.isFinite(gain)) {
          return;
        }
        const clampedGain = Math.min(2, Math.max(0, gain));
        sendOmniphonyFloatControl(`/omniphony/control/object/${Math.floor(id)}/gain`, clampedGain);
      }

      if (payload?.type === 'control:speaker:gain') {
        const id = Number(payload.id);
        const gain = Number(payload.gain);
        if (!Number.isFinite(id) || id < 0 || !Number.isFinite(gain)) {
          return;
        }
        const clampedGain = Math.min(2, Math.max(0, gain));
        sendOmniphonyFloatControl(`/omniphony/control/speaker/${Math.floor(id)}/gain`, clampedGain);
      }

      if (payload?.type === 'control:object:mute') {
        const id = Number(payload.id);
        const muted = Number(payload.muted);
        if (!Number.isFinite(id) || id < 0 || !Number.isFinite(muted)) {
          return;
        }
        sendOmniphonyIntControl(`/omniphony/control/object/${Math.floor(id)}/mute`, muted ? 1 : 0);
      }

      if (payload?.type === 'control:speaker:mute') {
        const id = Number(payload.id);
        const muted = Number(payload.muted);
        if (!Number.isFinite(id) || id < 0 || !Number.isFinite(muted)) {
          return;
        }
        sendOmniphonyIntControl(`/omniphony/control/speaker/${Math.floor(id)}/mute`, muted ? 1 : 0);
      }

      if (payload?.type === 'control:master:gain') {
        const gain = Number(payload.gain);
        if (!Number.isFinite(gain)) {
          return;
        }
        const clampedGain = Math.min(2, Math.max(0, gain));
        sendOmniphonyFloatControl('/omniphony/control/gain', clampedGain);
      }

      if (payload?.type === 'control:loudness') {
        const enable = Number(payload.enable);
        if (!Number.isFinite(enable)) {
          return;
        }
        sendOmniphonyIntControl('/omniphony/control/loudness', enable ? 1 : 0);
      }

      if (payload?.type === 'control:spread:min') {
        const value = Number(payload.value);
        if (!Number.isFinite(value)) {
          return;
        }
        const clamped = Math.min(1, Math.max(0, value));
        sendOmniphonyFloatControl('/omniphony/control/spread/min', clamped);
      }

      if (payload?.type === 'control:spread:max') {
        const value = Number(payload.value);
        if (!Number.isFinite(value)) {
          return;
        }
        const clamped = Math.min(1, Math.max(0, value));
        sendOmniphonyFloatControl('/omniphony/control/spread/max', clamped);
      }

      if (payload?.type === 'control:distance_diffuse:enabled') {
        const enable = Number(payload.enable);
        if (!Number.isFinite(enable)) return;
        sendOmniphonyIntControl('/omniphony/control/distance_diffuse/enabled', enable ? 1 : 0);
      }

      if (payload?.type === 'control:distance_diffuse:threshold') {
        const value = Number(payload.value);
        if (!Number.isFinite(value)) return;
        sendOmniphonyFloatControl('/omniphony/control/distance_diffuse/threshold', Math.max(0.01, value));
      }

      if (payload?.type === 'control:distance_diffuse:curve') {
        const value = Number(payload.value);
        if (!Number.isFinite(value)) return;
        sendOmniphonyFloatControl('/omniphony/control/distance_diffuse/curve', Math.max(0, value));
      }

      if (payload?.type === 'control:speaker:az') {
        const id = Number(payload.id);
        const value = Number(payload.value);
        if (!Number.isFinite(id) || id < 0 || !Number.isFinite(value)) {
          return;
        }
        sendOmniphonyFloatControl(`/omniphony/control/speaker/${Math.floor(id)}/az`, value);
      }

      if (payload?.type === 'control:speaker:el') {
        const id = Number(payload.id);
        const value = Number(payload.value);
        if (!Number.isFinite(id) || id < 0 || !Number.isFinite(value)) {
          return;
        }
        sendOmniphonyFloatControl(`/omniphony/control/speaker/${Math.floor(id)}/el`, value);
      }

      if (payload?.type === 'control:speaker:distance') {
        const id = Number(payload.id);
        const value = Number(payload.value);
        if (!Number.isFinite(id) || id < 0 || !Number.isFinite(value)) {
          return;
        }
        sendOmniphonyFloatControl(`/omniphony/control/speaker/${Math.floor(id)}/distance`, value);
      }

      if (payload?.type === 'control:speakers:apply') {
        sendOmniphonyNoArgs('/omniphony/control/speakers/apply');
      }

      if (payload?.type === 'control:save_config') {
        sendOmniphonyNoArgs('/omniphony/control/save_config');
      }
    } catch {
      // Ignore invalid client payloads.
    }
  });

  ws.send(
    JSON.stringify({
      type: 'state:init',
      sources: state.sources,
      sourceLevels: state.sourceLevels,
      speakerLevels: state.speakerLevels,
      objectSpeakerGains: state.objectSpeakerGains,
      objectGains: state.objectGains,
      speakerGains: state.speakerGains,
      objectMutes: state.objectMutes,
      speakerMutes: state.speakerMutes,
      roomRatio: state.roomRatio,
      spread: state.spread,
      loudness: state.loudness,
      loudnessSource: state.loudnessSource,
      loudnessGain: state.loudnessGain,
      masterGain: state.masterGain,
      distanceDiffuse: state.distanceDiffuse,
      configSaved: state.configSaved,
      latencyMs: state.latencyMs,
      resampleRatio: state.resampleRatio,
      layouts: state.layouts,
      selectedLayoutKey: state.selectedLayoutKey
    })
  );
});

server.listen(HTTP_PORT, () => {
  console.log(`[http] http://localhost:${HTTP_PORT}`);
});


let isShuttingDown = false;

function shutdown(signal) {
  if (isShuttingDown) {
    return;
  }
  isShuttingDown = true;

  if (signal) {
    console.log(`[shutdown] received ${signal}, stopping services...`);
  }

  stopHeartbeat();

  wss.clients.forEach((client) => {
    try {
      client.close();
    } catch {
      // Ignore close errors during shutdown.
    }
  });

  try {
    wss.close();
  } catch {
    // Ignore close errors during shutdown.
  }

  try {
    oscUdpPort.close();
  } catch {
    // Ignore close errors during shutdown.
  }

  server.close(() => {
    process.exit(0);
  });

  setTimeout(() => {
    process.exit(0);
  }, 1000).unref();
}

process.on('SIGINT', () => shutdown('SIGINT'));
process.on('SIGTERM', () => shutdown('SIGTERM'));

oscUdpPort.open();
