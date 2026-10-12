#!/usr/bin/env node

// One minimum supported Rust version, said the same way everywhere it is said.
//
// Why this exists: the workspace declared `rust-version = "1.85"`, the README
// badge and requirements said 1.95+, CONTRIBUTING said 1.95+, and the lockfile
// pinned five crates (webauthn-rs, jsonwebtoken, time, image, home) that refuse
// anything older than 1.88 (card_b5633973b38f). Three numbers, none of them
// checked, and the one cargo reads was wrong: a downstream builder on 1.86
// passed cargo's `rust-version` gate and then failed in a dependency.
//
// The number lives in `[workspace.package]` and every crate inherits it. This
// check asserts the inheritance and that the README and CONTRIBUTING state that
// number and no other. Whether the number is TRUE is the `msrv` job's question:
// it installs this exact toolchain and checks the workspace against the
// lockfile, reading the version from here (`--print`) rather than repeating it.
//
// Comments are not declarations. TOML `#` lines and HTML comments in Markdown
// are dropped before anything is read, so a commented-out old value can
// neither satisfy nor fail the check.
//
// Usage:
//   node scripts/msrv-declaration-contract-check.mjs           # the check
//   node scripts/msrv-declaration-contract-check.mjs --print   # also `version=<msrv>` on stdout

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const print = process.argv.includes('--print');
const problems = [];

/// TOML with every comment removed: whole-line comments, and a trailing `#`
/// outside a string on a value line (the manifests here never put `#` inside a
/// string, so the simple split is exact for them).
function tomlWithoutComments(path) {
  return readFileSync(path, 'utf8')
    .split('\n')
    .map((line) => {
      const hash = line.indexOf('#');
      if (hash === -1) return line;
      const before = line.slice(0, hash);
      const quotes = (before.match(/"/g) ?? []).length;
      return quotes % 2 === 0 ? before : line;
    })
    .join('\n');
}

function markdownWithoutComments(path) {
  return readFileSync(path, 'utf8').replace(/<!--[\s\S]*?-->/g, '');
}

function tomlTable(text, name) {
  const lines = text.split('\n');
  const start = lines.findIndex((line) => line.trim() === `[${name}]`);
  if (start === -1) return null;
  const end = lines.findIndex((line, index) => index > start && /^\s*\[/.test(line));
  return lines.slice(start + 1, end === -1 ? undefined : end).join('\n');
}

// ── The declaration ──
const manifest = tomlWithoutComments(join(root, 'Cargo.toml'));
const workspacePackage = tomlTable(manifest, 'workspace.package');
const declared = workspacePackage?.match(/^\s*rust-version\s*=\s*"([^"]+)"\s*$/m)?.[1];
if (!declared) {
  console.error('❌ Cargo.toml: [workspace.package] declares no `rust-version` — there is no MSRV to agree with.');
  process.exit(1);
}
if (!/^1\.\d+$/.test(declared)) {
  problems.push(
    `Cargo.toml: rust-version is "${declared}". Declare a minor version (\`1.NN\`): `
      + 'the badge, the docs and the msrv job all speak in minors.',
  );
}

// ── Every crate inherits it ──
const cratesDir = join(root, 'crates');
const crates = existsSync(cratesDir)
  ? readdirSync(cratesDir).filter((name) => existsSync(join(cratesDir, name, 'Cargo.toml'))).sort()
  : [];
if (crates.length === 0) {
  problems.push('crates/ holds no crate manifest — the read broke, not the workspace.');
}
for (const crate of crates) {
  const text = tomlWithoutComments(join(cratesDir, crate, 'Cargo.toml'));
  const ownPackage = tomlTable(text, 'package') ?? '';
  if (/^\s*rust-version\s*=/m.test(ownPackage)) {
    problems.push(`crates/${crate}/Cargo.toml declares its own rust-version — inherit the workspace one instead.`);
  } else if (!/^\s*rust-version\.workspace\s*=\s*true\s*$/m.test(ownPackage)) {
    problems.push(
      `crates/${crate}/Cargo.toml does not set \`rust-version.workspace = true\` — `
        + 'cargo would build it on any compiler at all.',
    );
  }
}

// ── The documents say the same number ──
//
// Each entry is a document and the statements it must make. Every statement
// has to be found at least once (a reworded sentence must not turn the check
// vacuous), and every version any of them names has to be the declared one.
const STATEMENTS = [
  {
    file: 'README.md',
    patterns: [
      { what: 'the Rust badge', regex: /img\.shields\.io\/badge\/rust-(\d+\.\d+)%2B/g },
      { what: 'the requirement', regex: /^- Rust (\d+\.\d+)\+/gm },
    ],
  },
  {
    file: 'CONTRIBUTING.md',
    patterns: [{ what: 'the required toolchain', regex: /^# Rust (\d+\.\d+)\+/gm }],
  },
];

for (const { file, patterns } of STATEMENTS) {
  const path = join(root, file);
  if (!existsSync(path)) {
    problems.push(`${file} is missing.`);
    continue;
  }
  const text = markdownWithoutComments(path);
  for (const { what, regex } of patterns) {
    const versions = [...text.matchAll(regex)].map((match) => match[1]);
    if (versions.length === 0) {
      problems.push(`${file} no longer states ${what} in the form this check reads (${regex}).`);
      continue;
    }
    for (const version of versions) {
      if (version !== declared) {
        problems.push(`${file}: ${what} says Rust ${version}+, Cargo.toml declares ${declared}.`);
      }
    }
  }
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

if (print) process.stdout.write(`version=${declared}\n`);
console.error(
  `msrv: Rust ${declared}, inherited by ${crates.length} crate(s), stated alike in README.md and CONTRIBUTING.md`,
);
