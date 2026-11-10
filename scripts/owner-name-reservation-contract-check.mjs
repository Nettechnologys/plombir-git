#!/usr/bin/env node

// A new owner namespace must not bypass LDAP merely because its row is a
// bot or an organization. This source ratchet complements real LDAP HTTP tests.
// The older registration flows keep their distinct validation/padding order;
// their local-only reads below are allowed only alongside a directory check.
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadProductionRust } from './lib/rust-consumer-contract.mjs';

function functions(source) {
  const found = [];
  const declarations = /^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)\b/gm;
  for (const match of source.matchAll(declarations)) {
    const open = source.indexOf('{', match.index);
    if (open < 0) continue;
    let end = open + 1;
    let depth = 1;
    while (end < source.length && depth) {
      if (source[end] === '{') depth++;
      if (source[end] === '}') depth--;
      end++;
    }
    found.push({ name: match[1], start: match.index, open, end, body: source.slice(open, end), header: source.slice(match.index, open) });
  }
  return found;
}

export function checkOwnerReservations(production) {
  const failures = [];
  const localReads = new Map([
    ['crates/rg-core/src/namespace.rs:owner_name_is_reserved', 'ldap_directory_holds'],
    ['crates/rg-core/src/user/service.rs:register', 'ldap_directory_holds'],
    ['crates/rg-core/src/user/service.rs:resolve_ldap_identity', null], // Already authenticating through this directory.
    ['crates/rg-core/src/user/account.rs:ensure_new_identity', null], // Shared with trusted host-side create-admin; HTTP create_account is checked below.
    ['crates/rg-core/src/user/account.rs:register_pending', 'ldap_directory_holds'],
    ['crates/rg-core/src/user/account.rs:confirm', 'ldap_directory_holds'],
    ['crates/rg-http/src/api/sso.rs:name_is_free', 'ldap_directory_holds'],
  ]);
  const required = new Map([
    ['crates/rg-core/src/org/mod.rs:create_org', 'owner_name_is_reserved'],
    ['crates/rg-core/src/user/bots.rs:create_bot', 'owner_name_is_reserved'],
    ['crates/rg-core/src/user/account.rs:create_account', 'ldap_directory_holds'],
  ]);
  const seen = new Set();
  for (const [file, source] of production) {
    const relative = path.relative(process.cwd(), path.resolve(file)).split(path.sep).join('/');
    const entries = functions(source);
    for (const call of source.matchAll(/\bowner_name_is_taken\s*\(/g)) {
      if (/\bfn\s+$/.test(source.slice(0, call.index))) continue;
      const fn = entries.find((entry) => entry.open < call.index && entry.end > call.index);
      const key = `${relative}:${fn?.name ?? '<unclassified>'}`;
      if (!localReads.has(key)) {
        failures.push(`${key}: local-only owner-name check; use owner_name_is_reserved with the instance directory context`);
        continue;
      }
      seen.add(key);
      const guard = localReads.get(key);
      if (guard && !new RegExp(`\\b${guard}\\s*\\(`).test(fn.body)) {
        failures.push(`${key}: missing ${guard}`);
      }
    }
    for (const fn of entries) {
      const key = `${relative}:${fn.name}`;
      if (!required.has(key)) continue;
      seen.add(key);
      if (!/\bdirectories\s*:\s*(?:\w+::)*LdapDirectories\s*</.test(fn.header)) {
        failures.push(`${key}: directory context must be a required service parameter`);
      }
      if (!new RegExp(`\\b${required.get(key)}\\s*\\(`).test(fn.body)) {
        failures.push(`${key}: missing ${required.get(key)}`);
      }
    }
  }
  for (const key of [...localReads.keys(), ...required.keys()]) {
    if (!seen.has(key)) failures.push(`${key}: stale owner-name reservation contract`);
  }
  return failures;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const failures = checkOwnerReservations(loadProductionRust(path.resolve('crates')));
  if (failures.length) {
    console.error(failures.join('\n'));
    process.exitCode = 1;
  } else {
    console.log('Owner-name reservations: directory guards and explicit trusted exceptions verified');
  }
}
