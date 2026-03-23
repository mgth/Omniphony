/**
 * Logging system — log panel, entries, level control.
 */

import { t, tf, i18nState } from './i18n.js';

const logOverlayEl = document.getElementById('logOverlay');
const logSummaryEl = document.getElementById('logSummary');
const logEntriesEl = document.getElementById('logEntries');
const logEmptyStateEl = document.getElementById('logEmptyState');
const logToggleBtnEl = document.getElementById('logToggleBtn');
const logLevelSelectEl = document.getElementById('logLevelSelect');

const LOG_ENTRY_LIMIT = 120;
export const LOG_LEVEL_VALUES = ['off', 'error', 'warn', 'info', 'debug', 'trace'];

export const logState = {
  expanded: false,
  entries: [],
  backendLogLevel: 'info'
};

function formatLogTime(date) {
  return new Intl.DateTimeFormat(i18nState.locale, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit'
  }).format(date);
}

function formatLogTimestampForExport(date) {
  const pad = (value) => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

export function serializeLogsForClipboard() {
  return logState.entries
    .map((entry) => `[${formatLogTimestampForExport(entry.timestamp)}] ${String(entry.level || 'info').toUpperCase()} ${entry.message}`)
    .join('\n');
}

export function normalizeLogError(error) {
  if (error instanceof Error) {
    return error.message || error.name || String(error);
  }
  if (typeof error === 'string') return error;
  try {
    return JSON.stringify(error);
  } catch (_e) {
    return String(error);
  }
}

export function normalizeLogLevel(value) {
  const normalized = String(value || '').trim().toLowerCase();
  return LOG_LEVEL_VALUES.includes(normalized) ? normalized : 'info';
}

export function pushLog(level, message) {
  logState.entries.push({
    id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    level: ['warn', 'error', 'debug', 'trace'].includes(level) ? level : 'info',
    message,
    timestamp: new Date()
  });
  if (logState.entries.length > LOG_ENTRY_LIMIT) {
    logState.entries.splice(0, logState.entries.length - LOG_ENTRY_LIMIT);
  }
  renderLogPanel();
}

export function renderLogLevelControl() {
  if (!logLevelSelectEl) return;
  const value = normalizeLogLevel(logState.backendLogLevel);
  if (logLevelSelectEl.value !== value) {
    logLevelSelectEl.value = value;
  }
}

export function renderLogPanel() {
  if (!logOverlayEl || !logSummaryEl || !logEntriesEl || !logEmptyStateEl || !logToggleBtnEl) return;
  const latest = logState.entries[logState.entries.length - 1];
  logOverlayEl.classList.toggle('expanded', logState.expanded);
  logOverlayEl.classList.toggle('collapsed', !logState.expanded);
  logSummaryEl.textContent = latest ? latest.message : t('log.empty');
  logToggleBtnEl.textContent = logState.expanded ? '▴' : '▾';
  logToggleBtnEl.title = t(logState.expanded ? 'log.collapse' : 'log.expand');
  logEmptyStateEl.style.display = logState.entries.length > 0 ? 'none' : 'block';
  logEntriesEl.innerHTML = '';
  if (logState.entries.length === 0) return;
  const fragment = document.createDocumentFragment();
  logState.entries.slice().reverse().forEach((entry) => {
    const row = document.createElement('div');
    row.className = 'log-entry';

    const timeEl = document.createElement('div');
    timeEl.className = 'log-entry-time';
    timeEl.textContent = formatLogTime(entry.timestamp);

    const levelEl = document.createElement('div');
    levelEl.className = `log-entry-level ${['warn', 'error', 'debug', 'trace'].includes(entry.level) ? entry.level : 'info'}`;
    levelEl.textContent = t(`log.level.${entry.level}`);

    const msgEl = document.createElement('div');
    msgEl.className = 'log-entry-message';
    msgEl.textContent = entry.message;

    row.appendChild(timeEl);
    row.appendChild(levelEl);
    row.appendChild(msgEl);
    fragment.appendChild(row);
  });
  logEntriesEl.appendChild(fragment);
}

export function setLogExpanded(next) {
  logState.expanded = Boolean(next);
  renderLogPanel();
}

export async function copyLogsToClipboard() {
  if (!logState.entries.length) {
    pushLog('warn', t('log.copyEmpty'));
    return;
  }
  const text = serializeLogsForClipboard();
  try {
    if (navigator?.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
    } else {
      const textarea = document.createElement('textarea');
      textarea.value = text;
      textarea.setAttribute('readonly', 'true');
      textarea.style.position = 'fixed';
      textarea.style.left = '-9999px';
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand('copy');
      document.body.removeChild(textarea);
    }
    pushLog('info', tf('log.copySuccess', { count: logState.entries.length }));
  } catch (error) {
    pushLog('error', tf('log.copyFailed', { error: normalizeLogError(error) }));
  }
}
