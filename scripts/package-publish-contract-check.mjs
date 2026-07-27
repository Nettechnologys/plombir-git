#!/usr/bin/env node

import { readFileSync } from 'node:fs';

import { requireBlock } from './lib/rust-source.mjs';

const source = readFileSync('web/src/lib/api/packages.ts', 'utf8');
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

// Both the regex and the indexOf slicing below used to fall back to an empty
// string, which the negative assertion at the end reads as "clean".
const packagesBlock = requireBlock(
  source,
  /export const packages = \{([\s\S]*?)\n\};/,
  'API client must export a packages object literal',
  failures,
  1,
);

const createStart = packagesBlock === null ? -1 : packagesBlock.indexOf('create:');
const createEnd = packagesBlock === null ? -1 : packagesBlock.indexOf('\n  delete:', createStart);

if (packagesBlock !== null && (createStart < 0 || createEnd <= createStart)) {
  failures.push('packages.create block could not be located between create: and delete: in the packages object');
}

const createSource = createStart >= 0 && createEnd > createStart ? packagesBlock.slice(createStart, createEnd) : null;

if (createSource !== null && !/packages\.publish\(/.test(createSource)) {
  failures.push('packages.create must delegate to packages.publish so it sends the backend octet-stream payload');
}

if (createSource !== null && /body:\s*JSON\.stringify\(data\)/.test(createSource)) {
  failures.push('packages.create must not JSON.stringify metadata to the binary package publish endpoint');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.error(`❌ ${failure}`);
  }
  process.exit(1);
}

console.log('Package publish frontend/backend contract ok');
