#!/usr/bin/env node
// Options-schema contract check (registry RFC phase 1, docs/live-options-registry.md).
//
// The renderer dumps its declared live options (`cargo run -p renderer
// --example dump_options_schema`); this script asserts every declared option
// is actually surfaceable by the Studio:
//   - the schema is well-formed (key, kind, default, flags),
//   - every i18nKey / helpI18nKey resolves in src/i18n/en.json (the reference
//     locale — per-locale parity is check-i18n.mjs's job).
//
// Unlike the warn-only i18n parity check, this is a hard gate: an option
// declared engine-side but invisible to the UI is exactly the silent-omission
// class the registry exists to kill.
//
// Usage: node scripts/check-options-schema.mjs <path-to-options-schema.json>

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const schemaPath = process.argv[2];
if (!schemaPath) {
  console.error('usage: check-options-schema.mjs <options-schema.json>');
  process.exit(2);
}

const here = dirname(fileURLToPath(import.meta.url));
const en = JSON.parse(readFileSync(join(here, '..', 'src', 'i18n', 'en.json'), 'utf8'));

// en.json uses flat, literally-dotted keys; tolerate nested objects too.
function resolvesInEn(key) {
  if (typeof en[key] === 'string') return true;
  let node = en;
  for (const part of key.split('.')) {
    if (node === null || typeof node !== 'object' || !(part in node)) return false;
    node = node[part];
  }
  return typeof node === 'string';
}

const KINDS = new Set(['bool', 'enum', 'string']);
const failures = [];
const fail = (msg) => {
  failures.push(msg);
  if (process.env.GITHUB_ACTIONS) console.log(`::error::options-schema: ${msg}`);
};

let schema;
try {
  schema = JSON.parse(readFileSync(schemaPath, 'utf8'));
} catch (e) {
  fail(`cannot read/parse ${schemaPath}: ${e.message}`);
  report();
}
if (!Array.isArray(schema) || schema.length === 0) {
  fail('schema is not a non-empty array');
  report();
}

const seen = new Set();
for (const spec of schema) {
  const key = spec.key;
  if (typeof key !== 'string' || !/^[a-z][a-z_]*$/.test(key)) {
    fail(`bad option key: ${JSON.stringify(key)}`);
    continue;
  }
  if (seen.has(key)) fail(`${key}: duplicate key`);
  seen.add(key);
  if (!KINDS.has(spec.kind)) fail(`${key}: unknown kind '${spec.kind}'`);
  if (spec.kind === 'enum') {
    if (!Array.isArray(spec.values) || spec.values.length < 2) {
      fail(`${key}: enum without >= 2 values`);
    } else if (!spec.values.includes(spec.default)) {
      fail(`${key}: default '${spec.default}' not in values`);
    }
  }
  if (spec.default === undefined) fail(`${key}: missing default`);
  if (!Array.isArray(spec.flags)) fail(`${key}: missing flags`);
  if (typeof spec.i18nKey !== 'string' || !resolvesInEn(spec.i18nKey)) {
    fail(`${key}: i18nKey '${spec.i18nKey}' does not resolve in en.json`);
  }
  if (spec.helpI18nKey !== undefined && !resolvesInEn(spec.helpI18nKey)) {
    fail(`${key}: helpI18nKey '${spec.helpI18nKey}' does not resolve in en.json`);
  }
}

report();

function report() {
  if (failures.length) {
    console.error(`options-schema contract: ${failures.length} failure(s)`);
    for (const f of failures) console.error(`  - ${f}`);
    process.exit(1);
  }
  console.log(`options-schema contract: OK (${schema.length} declared options)`);
  process.exit(0);
}
