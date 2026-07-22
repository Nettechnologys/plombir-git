#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const settingsPath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/+page.svelte');
const enPath = path.join(root, 'web/src/lib/i18n/translations/en.json');
const zhPath = path.join(root, 'web/src/lib/i18n/translations/zh-CN.json');

const settings = readFileSync(settingsPath, 'utf8');
const en = JSON.parse(readFileSync(enPath, 'utf8'));
const zh = JSON.parse(readFileSync(zhPath, 'utf8'));

const failures = [];

if (!/repositoryPath\s*=\s*\$derived\(`\$\{owner\}\/\$\{repo\}`\)/.test(settings)) {
  failures.push('Settings page must derive the full owner/repo path for destructive repository actions.');
}

if (/deleteConfirm\s*!==\s*repo(?![A-Za-z0-9_$])/.test(settings)) {
  failures.push('Delete confirmation must not accept bare repository names.');
}

if (!/deleteConfirm\s*!==\s*repositoryPath/.test(settings)) {
  failures.push('Delete button and handler must require the full repository path.');
}

if (!/confirm_instruction',\s*\{\s*repo:\s*repositoryPath\s*\}/.test(settings)) {
  failures.push('Delete confirmation copy must display the exact owner/repo path the backend route deletes.');
}

if (en.settings.delete.confirm_placeholder !== 'Type owner/repository') {
  failures.push('English delete confirmation placeholder must ask for owner/repository.');
}

if (zh.settings.delete.confirm_placeholder !== '输入 owner/repository') {
  failures.push('Chinese delete confirmation placeholder must ask for owner/repository.');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Repo settings frontend/backend contract ok');
