// i18n parity checker for Omniphony Studio.
//
// Compares every locale file under src/i18n/ against en.json (the reference)
// and reports three classes of problem:
//   - missing:    a key present in en.json but absent from the locale
//   - orphaned:   a key present in the locale but absent from en.json
//   - leftover:   a translatable value left byte-identical to English
//
// Warn-only by default (exit 0) so it never blocks CI; pass --strict to make
// any problem exit non-zero for local enforcement. In GitHub Actions it also
// emits ::warning:: annotations so problems surface on the PR.
//
//   node scripts/check-i18n.mjs            # report, exit 0
//   node scripts/check-i18n.mjs --strict   # report, exit 1 if any problem

import { readFileSync, readdirSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const i18nDir = join(scriptDir, '..', 'src', 'i18n');
const REFERENCE = 'en';

const strict = process.argv.includes('--strict');
const inActions = Boolean(process.env.GITHUB_ACTIONS);

// Values that are intentionally identical across locales (format templates,
// product name) are not "leftovers". Extend as needed when a translation
// deliberately keeps the English string.
const LEFTOVER_ALLOW_KEYS = new Set([
  'app.title',
  'audio.summary',
  'status.audioFormat',
  'status.latencyFallback',
  // Feature/proper names and literal tokens kept verbatim in every locale.
  'distance.title',
  'distance.infoTitle',
  'spread.mode.objectSize',
  'autoTune.openButton',
  'vbap.cart',
  'vbap.polar',
  'audio.rampModeFrame',
  'adaptive.controlSmoothingOrder.opt2',
  // "Spread" is kept as a loanword in the Latin-script locales, so "Spread
  // min"/"Spread max" are legitimately identical to English there.
  'backendParam.spread_min',
  'backendParam.spread_max',
  // Literal format token and a proper name (Spagnol's PRTF model), kept
  // verbatim in every locale.
  'audio.formatCaf',
  'binaural.hrtfSource.prtf',
  // "File", "Stream" and "named pipe" are standard loanwords in Italian, so
  // these labels are legitimately identical to English there.
  'audio.backendFile',
  'audio.namedPipe'
]);
const LEFTOVER_ALLOW_PREFIXES = ['renderer.perf.'];

// A value is "translatable" (so an identical copy counts as a leftover) only if
// it carries real prose: at least two alphabetic words of length >= 3 once
// placeholders ({x}), HTML tags and punctuation are stripped.
function isTranslatable(value) {
  if (typeof value !== 'string') return false;
  const prose = value
    .replace(/\{[^}]*\}/g, ' ')
    .replace(/<[^>]*>/g, ' ')
    .replace(/[^\p{L}\s]/gu, ' ');
  const words = prose.split(/\s+/).filter((w) => w.length >= 3);
  return words.length >= 2;
}

function isLeftoverAllowed(key) {
  if (LEFTOVER_ALLOW_KEYS.has(key)) return true;
  return LEFTOVER_ALLOW_PREFIXES.some((p) => key.startsWith(p));
}

function loadLocale(name) {
  const text = readFileSync(join(i18nDir, `${name}.json`), 'utf8');
  return JSON.parse(text);
}

const localeFiles = readdirSync(i18nDir)
  .filter((f) => f.endsWith('.json'))
  .map((f) => f.slice(0, -5));

const en = loadLocale(REFERENCE);
const enKeys = Object.keys(en);
const enKeySet = new Set(enKeys);

let totalProblems = 0;
const lines = [];

for (const locale of localeFiles) {
  if (locale === REFERENCE) continue;
  const data = loadLocale(locale);
  const keys = new Set(Object.keys(data));

  const missing = enKeys.filter((k) => !keys.has(k));
  const orphaned = [...keys].filter((k) => !enKeySet.has(k));
  const leftover = [...keys].filter(
    (k) =>
      enKeySet.has(k) &&
      data[k] === en[k] &&
      isTranslatable(en[k]) &&
      !isLeftoverAllowed(k)
  );

  const count = missing.length + orphaned.length + leftover.length;
  totalProblems += count;

  lines.push(
    `\n${locale}: ${missing.length} missing, ${orphaned.length} orphaned, ${leftover.length} leftover`
  );
  const sample = (arr) => arr.slice(0, 15).join(', ') + (arr.length > 15 ? ', …' : '');
  if (missing.length) lines.push(`  missing:  ${sample(missing)}`);
  if (orphaned.length) lines.push(`  orphaned: ${sample(orphaned)}`);
  if (leftover.length) lines.push(`  leftover: ${sample(leftover)}`);

  if (inActions && count) {
    console.log(
      `::warning title=i18n ${locale}::${missing.length} missing, ${orphaned.length} orphaned, ${leftover.length} untranslated keys (run \`npm run i18n:check\`)`
    );
  }
}

console.log(`i18n parity vs ${REFERENCE}.json (${enKeys.length} keys)`);
console.log(lines.join('\n'));
console.log(
  totalProblems === 0
    ? '\nAll locales are in parity.'
    : `\n${totalProblems} problem(s) across locales.`
);

if (strict && totalProblems > 0) process.exit(1);
