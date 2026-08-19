#!/usr/bin/env node

// Reject the released-ephemeral-port fixture shape across the workspace.
//
// Binding `127.0.0.1:0` only allocates a port while that listener is alive.
// Recording `local_addr()`, dropping the listener, and then connecting or
// asking another process to rebind the number creates a TOCTOU race with every
// other process on the host. Real test servers must move the owned listener
// into their server task; outage fixtures must keep a listener that produces a
// deterministic protocol/transport failure.
//
// Truth boundary: this catches the concrete Rust spelling that caused the live
// flakes (`TcpListener::bind` + `local_addr` + explicit `drop` of the same
// binding), common free-port helper names, and shell/Python getsockname probes.
// It is intentionally not a Rust ownership parser; the behavioural tests still
// prove that the replacement fixtures fail at the transport boundary.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { stripRustNonCode } from './lib/rust-consumer-contract.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.env.FORGEKEEP_RELEASED_PORT_ROOT || join(scriptsDir, '..'));
const failures = [];

function sourceFiles(dir, extensions) {
  if (!existsSync(dir)) {
    failures.push(`${relative(root, dir)}/ is missing, so the released-port sweep is incomplete`);
    return [];
  }

  const files = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...sourceFiles(path, extensions));
    } else if (extensions.some((extension) => entry.name.endsWith(extension))) {
      files.push(path);
    }
  }
  return files;
}

function escaped(identifier) {
  return identifier.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

for (const file of sourceFiles(join(root, 'crates'), ['.rs'])) {
  // Scan executable Rust only. This sweep is the negative twin of the checks
  // that grep for a construct they *require*: there a comment produces a false
  // green, here it produced a false red. A commented-out fixture — the usual
  // residue of removing one — reported a released port that no test binds, and
  // so did a `drop(listener)` written inside a doc example or a raw string. The
  // view is byte-aligned and newline-preserving, so the line numbers this
  // reports still address the original file. `#[cfg(test)]` items are
  // deliberately kept: test fixtures are exactly what this hunts.
  const source = stripRustNonCode(readFileSync(file, 'utf8'));
  const lines = source.split('\n');

  for (let lineIndex = 0; lineIndex < lines.length; lineIndex += 1) {
    const drops = lines[lineIndex].matchAll(
      /\b(?:std::mem::)?drop\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)/g,
    );
    for (const match of drops) {
      const identifier = match[1];
      const start = Math.max(0, lineIndex - 80);
      const beforeDrop = lines.slice(start, lineIndex + 1).join('\n');
      const name = escaped(identifier);
      const bound = new RegExp(
        `\\blet\\s+(?:mut\\s+)?${name}\\s*=\\s*(?:(?!;)[\\s\\S])*?TcpListener::bind\\s*\\(`,
      ).test(beforeDrop);
      const addressRead = new RegExp(`\\b${name}\\s*\\.\\s*local_addr\\s*\\(`).test(beforeDrop);

      if (bound && addressRead) {
        failures.push(
          `${relative(root, file)}:${lineIndex + 1}: \`${identifier}\` is dropped after its ` +
            'ephemeral address was recorded; move the listener into the consumer instead of reusing the number',
        );
      }
    }
  }

  const suspiciousHelper = source.match(
    /\b(?:async\s+)?fn\s+((?:free|unused|available|ephemeral)_?port|dead_server_url)\b[\s\S]{0,2500}?TcpListener::bind\s*\([\s\S]{0,500}?local_addr\s*\(/,
  );
  if (suspiciousHelper) {
    const line = source.slice(0, suspiciousHelper.index).split('\n').length;
    failures.push(
      `${relative(root, file)}:${line}: helper \`${suspiciousHelper[1]}\` derives a reusable number from an ephemeral listener`,
    );
  }
}

for (const file of sourceFiles(join(root, 'scripts'), ['.sh', '.py'])) {
  const source = readFileSync(file, 'utf8');
  const probe = source.match(
    /socket\.socket\s*\([\s\S]{0,1000}?\.bind\s*\(\s*\(\s*["']127\.0\.0\.1["']\s*,\s*0\s*\)\s*\)[\s\S]{0,1000}?getsockname\s*\(/,
  );
  if (probe) {
    const line = source.slice(0, probe.index).split('\n').length;
    failures.push(
      `${relative(root, file)}:${line}: socket probe publishes an ephemeral port after the probe process releases it`,
    );
  }
}

if (failures.length > 0) {
  console.error('❌ released-port contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log('✅ released-port contract: no fixture releases an ephemeral listener and reuses its number');
