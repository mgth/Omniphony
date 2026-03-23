/**
 * Internationalization (i18n) module.
 */

import enTranslations from './i18n/en.json';
import frTranslations from './i18n/fr.json';
import deTranslations from './i18n/de.json';
import jaTranslations from './i18n/ja.json';
import esTranslations from './i18n/es.json';
import itTranslations from './i18n/it.json';
import ptBrTranslations from './i18n/pt-BR.json';
import zhCnTranslations from './i18n/zh-CN.json';

const LOCALE_STORAGE_KEY = 'spatialviz.locale';

const TRANSLATIONS = {
  en: enTranslations,
  fr: frTranslations,
  de: { ...enTranslations, ...deTranslations },
  ja: { ...enTranslations, ...jaTranslations },
  es: { ...enTranslations, ...esTranslations },
  it: { ...enTranslations, ...itTranslations },
  'pt-BR': { ...enTranslations, ...ptBrTranslations },
  'zh-CN': { ...enTranslations, ...zhCnTranslations }
};

export const LOCALE_OPTION_SPECS = [
  { value: 'auto', english: 'Auto', native: 'Auto' },
  { value: 'en', english: 'English', native: 'English' },
  { value: 'fr', english: 'French', native: 'Français' },
  { value: 'de', english: 'German', native: 'Deutsch' },
  { value: 'ja', english: 'Japanese', native: '日本語' },
  { value: 'es', english: 'Spanish', native: 'Español' },
  { value: 'it', english: 'Italian', native: 'Italiano' },
  { value: 'pt-BR', english: 'Portuguese (Brazil)', native: 'Português (Brasil)' },
  { value: 'zh-CN', english: 'Chinese (Simplified)', native: '简体中文' }
];

export { LOCALE_STORAGE_KEY };

function normalizeLocale(value) {
  return ['fr', 'de', 'ja', 'es', 'it', 'pt-BR', 'zh-CN'].includes(value) ? value : 'en';
}

export function normalizeLocalePreference(value) {
  if (value === 'auto') return 'auto';
  return normalizeLocale(value);
}

function detectSystemLocale() {
  const candidates = Array.isArray(navigator.languages) && navigator.languages.length > 0
    ? navigator.languages
    : [navigator.language].filter(Boolean);
  for (const candidate of candidates) {
    const normalized = String(candidate || '').toLowerCase();
    if (normalized.startsWith('fr')) return 'fr';
    if (normalized.startsWith('de')) return 'de';
    if (normalized.startsWith('ja')) return 'ja';
    if (normalized.startsWith('es')) return 'es';
    if (normalized.startsWith('it')) return 'it';
    if (normalized === 'pt-br' || normalized.startsWith('pt-br')) return 'pt-BR';
    if (normalized === 'zh-cn' || normalized.startsWith('zh-cn')) return 'zh-CN';
    if (normalized.startsWith('en')) return 'en';
  }
  return 'en';
}

function detectLocale() {
  const saved = localStorage.getItem(LOCALE_STORAGE_KEY);
  if (saved) {
    const pref = normalizeLocalePreference(saved);
    return pref === 'auto' ? detectSystemLocale() : pref;
  }
  return detectSystemLocale();
}

/** Current active locale — mutable so it can be changed at runtime. */
export const i18nState = { locale: detectLocale() };

export function setLocale(newLocale) {
  i18nState.locale = normalizeLocale(newLocale);
}

export function t(key) {
  return TRANSLATIONS[i18nState.locale]?.[key] ?? TRANSLATIONS.en[key] ?? key;
}

export function tf(key, values = {}) {
  const template = t(key);
  return String(template).replace(/\{(\w+)\}/g, (_, name) => {
    const value = values[name];
    return value === undefined || value === null ? '' : String(value);
  });
}

/**
 * Apply data-i18n, data-i18n-title, data-i18n-html attributes and locale select.
 * `renderLogLevelControl` and `renderLogPanel` are passed as callbacks to avoid
 * circular imports with the log module.
 */
export function applyStaticTranslations(renderLogLevelControl, renderLogPanel) {
  const localeSelectEl = document.getElementById('localeSelect');
  document.documentElement.lang = i18nState.locale;
  if (localeSelectEl) {
    const saved = localStorage.getItem(LOCALE_STORAGE_KEY);
    localeSelectEl.value = normalizeLocalePreference(saved || 'auto');
    LOCALE_OPTION_SPECS.forEach(({ value, english, native }) => {
      const option = localeSelectEl.querySelector(`option[value="${value}"]`);
      if (!option) return;
      option.textContent = native === english ? english : `${english} / ${native}`;
    });
  }
  document.querySelectorAll('[data-i18n]').forEach((el) => {
    const key = el.getAttribute('data-i18n');
    if (key) el.textContent = t(key);
  });
  document.querySelectorAll('[data-i18n-title]').forEach((el) => {
    const key = el.getAttribute('data-i18n-title');
    if (key) el.setAttribute('title', t(key));
  });
  document.querySelectorAll('[data-i18n-html]').forEach((el) => {
    const key = el.getAttribute('data-i18n-html');
    if (key) el.innerHTML = t(key);
  });
  if (renderLogLevelControl) renderLogLevelControl();
  if (renderLogPanel) renderLogPanel();
}
