#!/usr/bin/env node

// Under `pipefail`, a pipe into a reader that stops early reports the WRITER's
// status, not the reader's.
//
// `writer | grep -q x` ends grep on the first match; a writer that still has
// output to give then dies of SIGPIPE (141, or curl's 23), pipefail hands that
// status to the pipeline, and `if writer | grep -q x` reads "found" as "not
// found". It stays green while the output fits one pipe buffer, which is why
// it only shows up once a log or a file list grows: `scripts/trial-smoke.sh`
// failed on a `docker logs` past 64 KB with the line it looked for right there,
// and the image-build scope step in regression.yml would have skipped the
// build for a pull request long enough to list a thousand files.
// `writer | head -n 1` is the same race, and inside `$(…)` under `set -e` its
// no-match case exits the script before the error message is printed.
//
// The fix at every site is to read the output whole and search it from a
// here-string or a file. This check fails on any pipe into `head` or into a
// `grep` that stops early (`-q`, `-m`, `-l`, `-L` and their long forms) in a
// shell body that runs under pipefail: a tracked `*.sh` that turns it on or is
// sourced by one that does, and a workflow step that turns it on or runs under
// `shell: bash` (which GitHub starts as `bash -eo pipefail`).

import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseWorkflowFile, selectWorkflowParser } from './lib/workflow.mjs';
import { shellCodeOnly } from './lib/shell-source.mjs';

if (process.argv.length > 3) {
  console.error('Usage: node scripts/pipefail-early-reader-contract-check.mjs [repository-root]');
  process.exit(2);
}

const defaultRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = resolve(process.argv[2] ?? defaultRoot);

const EARLY_GREP_SHORT = /[qlLm]/;
const EARLY_GREP_LONG = /^--(quiet|silent|max-count(=.*)?|files-with-matches|files-without-match)$/;

function tracked(pattern) {
  const result = spawnSync('git', ['ls-files', '-z', '--', pattern], { cwd: root, encoding: 'utf8' });
  if (result.status !== 0) {
    console.error(`git ls-files ${pattern} failed in ${root}: ${result.stderr}`);
    process.exit(1);
  }
  return result.stdout.split('\0').filter(Boolean);
}

const runsUnderPipefail = (code) => /\bpipefail\b/.test(code);

// The grep options a reader was given, up to `--`. Quoted words are the
// pattern or a file name, never an option.
function grepStopsEarly(args) {
  for (const word of args.trim().split(/\s+/)) {
    if (word === '--') return false;
    if (word.startsWith('--')) {
      if (EARLY_GREP_LONG.test(word)) return true;
    } else if (/^-[A-Za-z]/.test(word)) {
      // `-A3` / `-m1`: the letters before the first digit are the flags.
      if (EARLY_GREP_SHORT.test(word.slice(1).split(/[0-9]/, 1)[0])) return true;
    }
  }
  return false;
}

// Blank what quotes hold, byte for byte, so a `|` in a message is not a pipe —
// except a `$(…)` inside double quotes, which is code again:
// `x="$(grep … | head -n 1)"` is the commonest shape this check exists for.
function blankQuotedText(code) {
  const out = [...code];
  // Each frame: 'code' (with its open-paren depth), "'" or '"'.
  const stack = [{ kind: 'code', depth: 0 }];
  for (let index = 0; index < code.length; index += 1) {
    const char = code[index];
    const frame = stack[stack.length - 1];
    if (frame.kind === "'") {
      if (char === "'") stack.pop();
      else if (char !== '\n') out[index] = ' ';
      continue;
    }
    if (frame.kind === '"') {
      if (char === '\\') {
        out[index] = ' ';
        if (code[index + 1] !== '\n') out[index + 1] = ' ';
        index += 1;
      } else if (char === '"') {
        stack.pop();
      } else if (char === '$' && code[index + 1] === '(') {
        stack.push({ kind: 'code', depth: 0 });
        index += 1;
      } else if (char !== '\n') {
        out[index] = ' ';
      }
      continue;
    }
    if (char === '\\') {
      index += 1;
    } else if (char === "'" || char === '"') {
      stack.push({ kind: char });
    } else if (char === '(') {
      frame.depth += 1;
    } else if (char === ')') {
      if (frame.depth > 0) frame.depth -= 1;
      else if (stack.length > 1) stack.pop();
    }
  }
  return out.join('');
}

// Every pipe into an early reader in `code`, a comment-free shell view. A
// continuation keeps its two bytes so offsets still map onto source lines.
function earlyReaders(code) {
  const folded = code.replace(/\\\n/g, '  ');
  const view = blankQuotedText(code).replace(/\\\n/g, '  ');
  const found = [];
  const pipe = /(?<!\|)\|(?![|&])[ \t\n]*(grep|head)\b([^\n|;&<>)]*)/g;
  for (const match of view.matchAll(pipe)) {
    const [, reader, args] = match;
    if (reader === 'grep' && !grepStopsEarly(args)) continue;
    // Report the whole logical line, numbered where it starts in `code`.
    const start = view.lastIndexOf('\n', match.index - 1) + 1;
    const end = view.indexOf('\n', match.index);
    const line = code.slice(0, start).split('\n').length;
    const text = folded.slice(start, end < 0 ? undefined : end).replace(/\s+/g, ' ').trim();
    found.push({ line, text });
  }
  return found;
}

const problems = [];

// ── Shell scripts ─────────────────────────────────────────────────────
const scripts = new Map();
for (const file of tracked('*.sh')) {
  scripts.set(file, shellCodeOnly(readFileSync(join(root, file), 'utf8')));
}
if (scripts.size === 0) {
  console.error('No tracked *.sh found — the listing is broken, not the repo.');
  process.exit(1);
}

const pipefailScripts = new Set([...scripts].filter(([, code]) => runsUnderPipefail(code)).map(([file]) => file));
// A library has no `set` of its own; it runs under whatever its sourcer set.
for (const file of [...pipefailScripts]) {
  for (const match of scripts.get(file).matchAll(/(?:^|[\s;])(?:source|\.)\s+"?[^"\s]*?([\w.-]+(?:\/[\w.-]+)*\.sh)"?/gm)) {
    // `"${ROOT_DIR}/scripts/lib/stand.sh"` names the tracked path by its tail.
    const named = match[1].replace(/^(\.?\/)+/, '');
    const sourced = [...scripts.keys()].find((candidate) => candidate === named || candidate.endsWith(`/${named}`) || named.endsWith(`/${candidate}`));
    if (sourced) pipefailScripts.add(sourced);
  }
}

for (const file of [...pipefailScripts].sort()) {
  for (const { line, text } of earlyReaders(scripts.get(file))) {
    problems.push(`${file}:${line}: ${text}`);
  }
}

// ── Workflow steps ────────────────────────────────────────────────────
const workflows = tracked('.github/workflows/*.yml');
const { parser, missing } = workflows.length > 0 ? selectWorkflowParser() : { parser: null, missing: [] };
if (workflows.length > 0 && !parser) {
  console.error(`No YAML parser available (tried ${missing.join(', ')}) — cannot inspect workflow steps.`);
  process.exit(1);
}

for (const file of workflows) {
  const parsed = parseWorkflowFile(parser, join(root, file));
  if (!parsed.ok) {
    console.error(`Cannot inspect ${file} — ${parsed.message ?? parsed.diagnostic}`);
    process.exit(1);
  }
  const workflowShell = parsed.document?.defaults?.run?.shell;
  for (const [jobName, job] of Object.entries(parsed.jobs ?? {})) {
    const jobShell = job?.defaults?.run?.shell ?? workflowShell;
    for (const [index, step] of (Array.isArray(job?.steps) ? job.steps : []).entries()) {
      if (typeof step?.run !== 'string') continue;
      const code = shellCodeOnly(step.run);
      const shell = step.shell ?? jobShell;
      if (shell !== 'bash' && !runsUnderPipefail(code)) continue;
      const label = step.name ?? `step ${index + 1}`;
      for (const { text } of earlyReaders(code)) {
        problems.push(`${file} job \`${jobName}\`, step "${label}": ${text}`);
      }
    }
  }
}

if (problems.length > 0) {
  console.error('A pipe into an early-exit reader runs under pipefail — read the output whole, then search it:');
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

console.log(
  `✅ no pipe into an early-exit reader under pipefail (${pipefailScripts.size} scripts, ${workflows.length} workflows)`,
);
