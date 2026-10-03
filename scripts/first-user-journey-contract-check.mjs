#!/usr/bin/env node

// Cheap wiring ratchet for the expensive browser journey. Runtime acceptance
// is still the authority; this check makes it hard to silently turn two fresh
// stands into one reused database or to pre-create the account the UI is meant
// to register.

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { shellCodeOnly } from './lib/shell-source.mjs';
import { productionTsSource } from './lib/ts-source.mjs';

const root = resolve(process.env.PLOMBIR_GIT_FIRST_USER_JOURNEY_ROOT || join(dirname(fileURLToPath(import.meta.url)), '..'));
const failures = [];

function read(path) {
  try { return readFileSync(join(root, path), 'utf8'); } catch (error) {
    failures.push(`${path} cannot be read: ${error.message}`);
    return '';
  }
}

function requireMatch(source, pattern, message) {
  if (!pattern.test(source)) failures.push(message);
}

const runner = shellCodeOnly(read('scripts/first-user-journey-e2e.sh'));
const stand = shellCodeOnly(read('scripts/ephemeral-stand.sh'));
const browser = productionTsSource(read('scripts/first-user-journey-e2e.mjs'));
let packageJson = {};
try { packageJson = JSON.parse(read('web/package.json')); } catch (error) {
  failures.push(`web/package.json is invalid JSON: ${error.message}`);
}

requireMatch(runner, /JOURNEY_RUNS=\$\{JOURNEY_RUNS:-2\}/, 'journey runner no longer defaults to two clean stands');
requireMatch(runner, /seq 1 "\$\{JOURNEY_RUNS\}"/, 'journey runner no longer loops over every requested clean stand');
requireMatch(runner, /STAND_REBUILD_FRONTEND=1\s+"\$\{ROOT_DIR\}\/scripts\/ephemeral-stand\.sh"/, 'journey runner may serve a stale web/build instead of the current frontend source');
requireMatch(runner, /ephemeral-stand\.sh"\s*\\\s*\n\s*--frontend\s*\\\s*\n\s*--no-founder\s*\\\s*\n\s*--\s*\\\s*\n\s*node .*first-user-journey-e2e\.mjs/s, 'journey runner must boot the frontend without pre-registering its user');

requireMatch(stand, /REGISTER_FOUNDER=1/, 'ephemeral stand lost the founder-registration default');
requireMatch(stand, /--no-founder\) REGISTER_FOUNDER=0/, 'ephemeral stand no longer accepts --no-founder');
requireMatch(stand, /if \[\[ \$\{REGISTER_FOUNDER\} -eq 1 \]\]; then\s*stand_register_founder/s, '--no-founder no longer controls the founder registration call');

for (const [pattern, message] of [
  [/input\[autocomplete=\\?"username\\?"\]/, 'browser journey no longer drives the registration/login username control'],
  [/Network\.getCookies/, 'browser journey no longer takes the real HttpOnly session into git'],
  [/git\(\['-C', seed, 'push'/, 'browser journey no longer performs a real git push'],
  [/a\.file-entry/, 'browser journey no longer opens a repository file through the blob link'],
  [/button\.btn-close/, 'browser journey no longer closes the created issue through the UI'],
]) requireMatch(browser, pattern, message);

if (packageJson.scripts?.['e2e:first-user'] !== 'bash ../scripts/first-user-journey-e2e.sh') {
  failures.push('web/package.json must expose the two-stand journey as e2e:first-user');
}

if (failures.length > 0) {
  console.error(`first-user journey contract failed (${failures.length}):`);
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log('✅ first-user journey contract: two clean stands, UI registration/login/repo/blob/issue, real git push');
