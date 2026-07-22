#!/usr/bin/env node

import { readFileSync } from 'node:fs';

const source = readFileSync('web/src/lib/api/client.svelte.ts', 'utf8');
const failures = [];

if (!/headers\[['"]Content-Disposition['"]\]\s*=\s*contentDispositionAttachment\(filename\)/.test(source)) {
  failures.push('packages.publish must build Content-Disposition through contentDispositionAttachment(filename)');
}

if (!/filename\*=UTF-8''\$\{encodeURIComponent\(/.test(source)) {
  failures.push('contentDispositionAttachment must use RFC 5987 filename*=UTF-8 percent encoding');
}

if (/Content-Disposition['"\]]\s*=\s*`attachment;\s*filename="\$\{filename\}"/.test(source)) {
  failures.push('packages.publish must not interpolate raw filenames into a quoted Content-Disposition header');
}

const packagesBlock = source.match(/export const packages = \{([\s\S]*?)\n\};/)?.[1] || '';
const createStart = packagesBlock.indexOf('create:');
const createEnd = packagesBlock.indexOf('\n  delete:', createStart);
const createSource = createStart >= 0 && createEnd > createStart ? packagesBlock.slice(createStart, createEnd) : '';
if (!/packages\.publish\(/.test(createSource)) {
  failures.push('packages.create must delegate to packages.publish so it sends the backend octet-stream payload');
}

if (/body:\s*JSON\.stringify\(data\)/.test(createSource)) {
  failures.push('packages.create must not JSON.stringify metadata to the binary package publish endpoint');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.error(`❌ ${failure}`);
  }
  process.exit(1);
}

console.log('Package publish frontend/backend contract ok');
