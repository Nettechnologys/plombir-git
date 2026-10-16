#!/usr/bin/env node

// The subcommands the two operator pages tell somebody to type, read once, for
// two consumers: this check, which holds them to the clap declarations in the
// same commit, and the release workflow, which asks the binaries it just built
// for each one's `--help`.
//
// Why this exists: `deploy/README.md` on main told operators to run
// `plombir-git create-admin` and to read a setup token from the log, and the
// latest release, v0.1.1, had neither — `unrecognized subcommand 'create-admin'`
// on a clean host (card_d670212c28bb). The pages describe main; what a person
// downloads is a tag. So the release gate asks the shipped binary about every
// subcommand its own pages name, and the push gate asks the declarations, so a
// page cannot name a command main does not have either.
//
// The same read caught a second way a page can hand over a command that does
// not run: `docker compose run --rm plombir-git restore-db …`. The image has no
// ENTRYPOINT, so whatever follows the service is the program, and Docker
// answers `exec: "restore-db": executable file not found in $PATH` — on the
// restore, migrate and key-rotation recipes, each run once, against a stopped
// server. A subcommand written straight after a compose service or an image
// reference is therefore a failure here, for as long as the Dockerfile's
// ENTRYPOINT is at most an init that execs its arguments (`tini --`, which
// reaps orphans as PID 1 — card_ca2b5f84cb69) — asserted below, so the premise
// cannot rot under the rule. Behind `tini --` the first word after the service
// is still the program.
//
// What counts as an invocation: inside a fenced code block, or an inline code
// span, the binary as a whole word (a `./target/release/` prefix allowed)
// followed by a lowercase word. Prose outside code is not a command. A token in
// a compose service or image position is not the binary, whatever its name.
//
// Output contract: stdout carries one `<binary> <subcommand>` line per distinct
// invocation and nothing else; everything a human reads goes to stderr; exit 1
// when any assertion fails.

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { productionRustCode } from './lib/rust-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.argv[2] ?? join(scriptsDir, '..'));

const PAGES = ['README.md', 'deploy/README.md'];
const BINARIES = [
  { binary: 'plombir-git', source: 'crates/rg-cli/src/cli.rs' },
  { binary: 'plombir-git-runner', source: 'crates/rg-runner/src/cli.rs' },
];
// What the pages must keep naming. A floor alone would let a renamed command
// pass by dropping out of the read; naming the ones that matter most is what
// makes that red.
const ANCHORS = [
  'plombir-git serve',
  'plombir-git migrate',
  'plombir-git create-admin',
  'plombir-git restore-db',
  'plombir-git rotate-encryption-key',
  'plombir-git-runner register',
  'plombir-git-runner run',
];
// Options of `docker run` / `docker compose run|exec` that take a value, so the
// value is not mistaken for the service or the image.
const VALUE_OPTIONS = new Set([
  '-e', '--env', '--env-file', '-w', '--workdir', '-u', '--user', '--entrypoint',
  '--name', '-p', '--publish', '-v', '--volume', '-l', '--label', '--network',
  '--mount', '--platform', '--pull', '--cpus', '--memory', '-m',
]);

const failures = [];

function read(relative) {
  const path = join(root, relative);
  if (!existsSync(path)) {
    failures.push(`${relative} is missing`);
    return null;
  }
  return readFileSync(path, 'utf8');
}

/** kebab-case, as clap names a `Subcommand` variant. */
function kebab(name) {
  return name.replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase();
}

/**
 * The subcommand names `enum Commands` declares, read from the code-only view
 * so a commented-out or `#[cfg(test)]` variant is not one.
 */
function declaredSubcommands(rustSource, label) {
  const code = productionRustCode(rustSource);
  const head = /\benum\s+Commands\s*\{/.exec(code);
  if (!head) {
    failures.push(`${label} no longer declares \`enum Commands\` where this check reads it`);
    return null;
  }
  // Keep only the depth-0 text of the enum body — variant names and the commas
  // between them. Field lists, tuple payloads and attribute bodies drop out,
  // and an attribute leaves only its `#`.
  let depth = 0;
  let surface = '';
  let index = head.index + head[0].length;
  for (; index < code.length; index += 1) {
    const char = code[index];
    if (char === '{' || char === '(' || char === '[') {
      depth += 1;
    } else if (char === '}' || char === ')' || char === ']') {
      if (depth === 0) break;
      depth -= 1;
    } else if (depth === 0) {
      surface += char;
    }
  }
  // clap adds `help` to every command that has subcommands.
  const names = new Set(['help']);
  for (const piece of surface.split(',')) {
    const match = /^([A-Z][A-Za-z0-9]*)\b/.exec(piece.replace(/#/g, ' ').trim());
    if (match) names.add(kebab(match[1]));
  }
  // A variant attribute could rename it (`#[command(name = …)]`) or add an
  // alias; the surface above drops attribute bodies, so look for them directly.
  const attributes = [...code.slice(head.index, index).matchAll(/#\s*\[\s*command\s*\(([^)]*)\)/g)]
    .map((match) => match[1])
    .filter((body) => /\b(name|alias|visible_alias|aliases)\b/.test(body));
  if (attributes.length > 0) {
    failures.push(`${label}: a subcommand carries \`#[command(${attributes[0].trim()})]\`, which renames or aliases it — teach this check to read it`);
  }
  if (names.size < 3) {
    failures.push(`${label}: only ${names.size - 1} subcommand(s) read out of \`enum Commands\` — the read broke, not the CLI`);
  }
  return names;
}

/** The code a markdown page shows: fenced blocks whole, inline spans of prose. */
function codeLines(markdown) {
  const lines = [];
  let fenced = false;
  let pending = null;
  markdown.split('\n').forEach((line, index) => {
    if (/^\s*(```|~~~)/.test(line)) {
      fenced = !fenced;
      return;
    }
    if (fenced) {
      // Backslash continuations make one logical command line.
      const text = pending ? `${pending.text} ${line.trim()}` : line;
      const start = pending ? pending.line : index + 1;
      if (/\\\s*$/.test(line)) {
        pending = { text: text.replace(/\\\s*$/, ''), line: start };
      } else {
        lines.push({ line: start, text });
        pending = null;
      }
      return;
    }
    for (const span of line.matchAll(/`([^`]+)`/g)) lines.push({ line: index + 1, text: span[1] });
  });
  if (pending) lines.push({ line: pending.line, text: pending.text });
  return lines;
}

/**
 * One command line split into simple commands (`&&`, `||`, `;`, `|`) and each
 * into words. Quotes and substitution punctuation become spaces, so
 * `"$(./target/release/plombir-git gen-secret)"` and the body of an
 * `sh -c '…'` read as the commands they contain. `<…>` stays attached: a
 * placeholder such as `<command>` is not a subcommand.
 */
function commands(text) {
  return text
    .split(/&&|\|\||;|\|/)
    .map((segment) => segment.replace(/['"`()[\]{},$]/g, ' ').split(/\s+/).filter(Boolean))
    .filter((words) => words.length > 0);
}

/**
 * Where a simple command's own program starts, for the `docker` wrappers the
 * pages use: after the service of `docker compose run|exec`, after the image of
 * `docker run`. Every other compose command (`logs`, `stop`, `up` …) names
 * services only, so nothing in it is the binary. `container` is the index of
 * the service or image, when there is one.
 */
function programStart(words) {
  const compose = words.findIndex((word) => word === 'compose' || word === 'docker-compose');
  const dockerRun = words[0] === 'docker' && words[1] === 'run' ? 1 : -1;
  if (compose === -1 && dockerRun === -1) return { from: 0, container: -1 };
  const verb = dockerRun !== -1
    ? dockerRun
    : words.findIndex((word, index) => index > compose && (word === 'run' || word === 'exec'));
  if (verb === -1) return { from: Infinity, container: -1 };
  for (let next = verb + 1; next < words.length; next += 1) {
    const word = words[next];
    if (word.startsWith('-')) {
      if (VALUE_OPTIONS.has(word)) next += 1;
      continue;
    }
    return { from: next + 1, container: next };
  }
  return { from: Infinity, container: -1 };
}

const declared = new Map();
for (const { binary, source } of BINARIES) {
  const text = read(source);
  if (text !== null) declared.set(binary, declaredSubcommands(text, source));
}
const everySubcommand = new Set([...declared.values()].flatMap((names) => (names ? [...names] : [])));
everySubcommand.delete('help');

// The one ENTRYPOINT that keeps "the word after the service is the program":
// an init that execs whatever follows `--`.
const PASS_THROUGH_ENTRYPOINT = 'ENTRYPOINT ["/usr/bin/tini", "--"]';
const dockerfile = read('Dockerfile');
const entrypoints = dockerfile === null
  ? []
  : dockerfile.split('\n').map((line) => line.trim()).filter((line) => /^ENTRYPOINT\b/i.test(line));
const entrypointPassesThrough = entrypoints.every((entrypoint) => entrypoint === PASS_THROUGH_ENTRYPOINT);
for (const entrypoint of entrypoints) {
  if (entrypoint === PASS_THROUGH_ENTRYPOINT) continue;
  failures.push(
    `Dockerfile now declares \`${entrypoint}\`, which is not the pass-through init \`${PASS_THROUGH_ENTRYPOINT}\`, `
      + 'so `docker compose run <service> <subcommand>` may be right and `<service> plombir-git <subcommand>` '
      + 'wrong — re-read the pages and this check\'s container-position rule',
  );
}

const invocations = new Map();
for (const page of PAGES) {
  const markdown = read(page);
  if (markdown === null) continue;
  const simpleCommands = codeLines(markdown)
    .flatMap(({ line, text }) => commands(text).map((words) => ({ line, words })));
  for (const { line, words } of simpleCommands) {
    const { from, container } = programStart(words);
    const next = words[container + 1];
    if (container !== -1 && entrypointPassesThrough && next && everySubcommand.has(next)) {
      failures.push(
        `${page}:${line}: \`${words[container]} ${next}\` — the image's ENTRYPOINT only execs its arguments, so Docker runs a program `
          + `called \`${next}\` and fails; write \`${words[container]} plombir-git ${next}\``,
      );
    }
    words.forEach((word, index) => {
      if (index < from) return;
      const binary = BINARIES.find(({ binary: name }) => word === name || word.endsWith(`/${name}`))?.binary;
      if (!binary) return;
      const sub = words[index + 1];
      if (!sub || !/^[a-z][a-z0-9-]*$/.test(sub) || BINARIES.some(({ binary: name }) => sub === name)) return;
      const key = `${binary} ${sub}`;
      if (!invocations.has(key)) invocations.set(key, []);
      invocations.get(key).push(`${page}:${line}`);
    });
  }
}

for (const [key, where] of invocations) {
  const [binary, sub] = key.split(' ');
  const names = declared.get(binary);
  if (names && !names.has(sub)) {
    failures.push(
      `${where.join(', ')}: \`${key}\` — ${binary} declares no \`${sub}\` subcommand; an operator pasting it gets `
        + '`unrecognized subcommand`. Fix the page, or restore the command.',
    );
  }
}

if (invocations.size < 10) {
  failures.push(`only ${invocations.size} distinct invocations read from ${PAGES.join(' and ')} — the read has stopped matching them`);
}
for (const anchor of ANCHORS) {
  if (!invocations.has(anchor)) {
    failures.push(`\`${anchor}\` no longer appears in a code span or block of ${PAGES.join(' or ')} — renamed on one side, or the read broke`);
  }
}

if (failures.length > 0) {
  console.error('❌ Operator-page subcommands contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

for (const key of [...invocations.keys()].sort()) console.log(key);
console.error(`✅ ${invocations.size} subcommand invocations on the operator pages, every one declared`);
