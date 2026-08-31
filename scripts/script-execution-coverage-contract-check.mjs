#!/usr/bin/env node

// Asserts that every script under `scripts/` is reachable from something that
// actually runs it — or carries a written reason why it is not.
//
// Why this exists: `scripts/observability-contract-check-regression.mjs` was
// executed by nothing for as long as it existed. It is the stand that proves the
// observability gate still asserts something, and it missed
// `run-contract-checks.mjs`'s glob by one hyphen: the glob matched
// `*-contract-check.mjs`, the stand ends in `-contract-check-regression.mjs`.
// Nothing was wrong with either file. The wiring was a *filename convention*,
// and a convention fails silently by construction — a file that does not match
// it is not reported as unmatched, it is simply never mentioned again.
//
// So the glob got widened, and this check exists so that the next widening is
// not needed to notice the next orphan. It does not care about names: it walks
// outward from the things an outside agent actually starts — CI workflows, git
// hooks, npm scripts — and asks which scripts that walk never reaches. A script
// the walk misses is either wired up, or listed in UNEXECUTED with a reason.
//
// Truth boundary, stated because it decides how to read a green run: workflows
// and package scripts contribute only their parsed shell bodies; shell files
// contribute comment-free command text; JavaScript contributes live module
// specifiers and filename values that flow into a process-launch call. This is
// still syntactic reachability, not proof that every runtime branch executes,
// but prose, comments and inert string literals cannot manufacture an edge.
//
// The subject is `scripts/` only. `deploy/*.sh` is operator tooling a human
// starts by hand — "nothing references it" is its normal state, not a defect,
// so folding it in here would make every run report a finding that is never
// actionable.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { basename, dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { shellCodeOnly } from './lib/shell-source.mjs';
import { productionTsCode, productionTsSource } from './lib/ts-source.mjs';
import {
  parseWorkflowFile,
  selectWorkflowParser,
  workflowJobRuns,
} from './lib/workflow.mjs';

const sourceScriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(
  process.env.FORGEKEEP_SCRIPT_EXECUTION_COVERAGE_ROOT ?? resolve(sourceScriptsDir, '..'),
);
const scriptsDir = join(root, 'scripts');
const SELF = basename(fileURLToPath(import.meta.url));
const RUNNER = 'run-contract-checks.mjs';

const failures = [];

// Scripts that nothing executes, and the reason each one does not have to.
//
// This is a ratchet, not a dumping ground — the same rules the contract-check
// runner's QUARANTINE lives by:
//   - an entry naming a file that no longer exists fails the check;
//   - an entry for a script that IS reachable fails the check (remove it);
//   - nothing goes in here without a reason a reader can act on.
//
// An exemption covers exactly the script it names and does not propagate to
// what that script calls. A manual tool's callees are equally manual, so they
// each owe their own line here — one entry quietly covering a subtree is how an
// exemption list stops describing the tree it exempts.
const UNEXECUTED = new Map([
  [
    'full-interface-regression.mjs',
    'a manual replay against a throwaway instance, and regression.yml says so where it runs the narrow ' +
      'routing-only smoke instead. It registers users, writes repositories and exercises delete verbs — ' +
      'nothing a shared runner should point at a real backend.',
  ],
  [
    'install-git-hooks.sh',
    'a one-time developer setup step (CONTRIBUTING.md tells you to run it). It installs the hook that ' +
      'runs the gates; a gate cannot install itself.',
  ],
]);

// The things an outside agent starts on its own. Everything else has to be
// reachable from one of these, or it is not run by anybody.
const ROOT_DIRS = ['.github/workflows', '.githooks'];
const ROOT_FILES = ['package.json', 'web/package.json'];

const roots = [];
for (const dir of ROOT_DIRS) {
  const full = join(root, dir);
  if (!existsSync(full)) {
    // A missing root is not "nothing to check" — it is this check losing the
    // half of the graph that dir represents, which would turn its verdicts into
    // noise. Say so instead of scoring the tree against what is left.
    failures.push(`${dir}/ does not exist, so every verdict below is computed from an incomplete root set`);
    continue;
  }
  for (const entry of readdirSync(full, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.isFile()) roots.push(join(full, entry.name));
  }
}
for (const file of ROOT_FILES) {
  const full = join(root, file);
  if (existsSync(full)) roots.push(full);
}

if (roots.length === 0) {
  console.error('❌ script execution coverage: no CI workflow, git hook or package.json found — nothing to walk from.');
  process.exit(1);
}

// The subjects: executables directly under `scripts/`, plus the modules under
// `scripts/lib/` they import (a library nobody imports is the same orphan one
// directory down).
function collect(dir, prefix) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    if (!entry.isFile()) continue;
    if (!entry.name.endsWith('.mjs') && !entry.name.endsWith('.sh')) continue;
    out.push(`${prefix}${entry.name}`);
  }
  return out;
}

const libDir = join(scriptsDir, 'lib');
const scripts = [...collect(scriptsDir, ''), ...(existsSync(libDir) ? collect(libDir, 'lib/') : [])];

// References are matched by basename, so two scripts sharing one would make
// every reference to either ambiguous. Refuse rather than attribute it to the
// first one found.
const byBasename = new Map();
for (const script of scripts) {
  const name = basename(script);
  if (byBasename.has(name)) {
    failures.push(
      `${byBasename.get(name)} and ${script} share a basename, so this check cannot tell which one a ` +
        'reference means — rename one of them',
    );
  }
  byBasename.set(name, script);
}

// The runner's globs are read out of the runner, not restated here: a copy of
// the naming rule in this file would drift from the real one, and drift in the
// wiring rule is the exact defect this check exists to catch.
const runnerPath = join(scriptsDir, RUNNER);
const runnerSource = existsSync(runnerPath) ? readFileSync(runnerPath, 'utf8') : '';
const runnerText = productionTsSource(runnerSource);
const runnerCode = productionTsCode(runnerSource);
const globSuffixes = [...runnerText.matchAll(/^const [A-Z_]*SUFFIX = '([^']+)';/gm)]
  .filter((match) => runnerCode.slice(match.index, match.index + 'const'.length) === 'const')
  .map((match) => match[1]);
if (globSuffixes.length === 0) {
  failures.push(
    `could not read any \`const …SUFFIX = '…'\` glob out of scripts/${RUNNER} — without them this check ` +
      'would report every contract check as an orphan, so it refuses to score the tree instead',
  );
}

function referencedIn(text) {
  return [...byBasename.entries()].filter(([name]) => text.includes(name)).map(([, script]) => script);
}

const JS_LAUNCHERS = new Set([
  'spawn',
  'spawnSync',
  'exec',
  'execSync',
  'execFile',
  'execFileSync',
  'fork',
  'Worker',
]);

function closingDelimiter(code, open, opening, closing) {
  let depth = 0;
  for (let index = open; index < code.length; index += 1) {
    if (code[index] === opening) depth += 1;
    if (code[index] === closing) depth -= 1;
    if (depth === 0) return index;
  }
  return null;
}

function splitCallArguments(code, text, open, close) {
  const argumentsFound = [];
  let start = open + 1;
  let paren = 0;
  let bracket = 0;
  let brace = 0;
  for (let index = start; index < close; index += 1) {
    const char = code[index];
    if (char === '(') paren += 1;
    else if (char === ')') paren -= 1;
    else if (char === '[') bracket += 1;
    else if (char === ']') bracket -= 1;
    else if (char === '{') brace += 1;
    else if (char === '}') brace -= 1;
    else if (char === ',' && paren === 0 && bracket === 0 && brace === 0) {
      argumentsFound.push({ code: code.slice(start, index), text: text.slice(start, index) });
      start = index + 1;
    }
  }
  argumentsFound.push({ code: code.slice(start, close), text: text.slice(start, close) });
  return argumentsFound;
}

function callsNamed(code, text, names) {
  if (names.size === 0) return [];
  const escaped = [...names].map((name) => name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
  const pattern = new RegExp(`(?<![A-Za-z0-9_$.])(${escaped.join('|')})\\s*\\(`, 'g');
  const calls = [];
  for (let match = pattern.exec(code); match !== null; match = pattern.exec(code)) {
    const open = code.indexOf('(', match.index + match[1].length);
    const close = closingDelimiter(code, open, '(', ')');
    if (close === null) continue;
    calls.push({ name: match[1], start: match.index, args: splitCallArguments(code, text, open, close) });
    pattern.lastIndex = close + 1;
  }
  return calls;
}

function identifierAppears(text, name) {
  return new RegExp(`(^|[^A-Za-z0-9_$])${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}([^A-Za-z0-9_$]|$)`)
    .test(text);
}

function scriptBindings(code, text) {
  const bindings = new Map();
  const declaration = /\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=/g;
  for (let match = declaration.exec(code); match !== null; match = declaration.exec(code)) {
    const end = code.indexOf(';', declaration.lastIndex);
    if (end < 0) continue;
    const scriptsInValue = referencedIn(text.slice(declaration.lastIndex, end));
    if (scriptsInValue.length > 0) bindings.set(match[1], scriptsInValue);
    declaration.lastIndex = end + 1;
  }
  return bindings;
}

function functionDefinitions(code, text) {
  const functions = [];
  const header = /\bfunction\s+([A-Za-z_$][\w$]*)\s*\(/g;
  for (let match = header.exec(code); match !== null; match = header.exec(code)) {
    const openParams = code.indexOf('(', match.index);
    const closeParams = closingDelimiter(code, openParams, '(', ')');
    if (closeParams === null) continue;
    const openBody = code.indexOf('{', closeParams + 1);
    if (openBody < 0) continue;
    const closeBody = closingDelimiter(code, openBody, '{', '}');
    if (closeBody === null) continue;
    const params = code.slice(openParams + 1, closeParams)
      .split(',')
      .map((param) => param.trim().match(/^([A-Za-z_$][\w$]*)/)?.[1] ?? null);
    functions.push({
      name: match[1],
      nameStart: match.index + match[0].indexOf(match[1]),
      params,
      code: code.slice(openBody + 1, closeBody),
      text: text.slice(openBody + 1, closeBody),
    });
    header.lastIndex = closeBody + 1;
  }
  return functions;
}

/**
 * Script references in executable JavaScript syntax.
 *
 * Arbitrary string literals are data, not reachability. A basename counts in a
 * module specifier, a process-launch argument, or a binding that flows into one
 * of those arguments. The keyword/callee must survive in the structure-only
 * view, so the same spelling inside a string or comment cannot manufacture an
 * edge.
 */
function javaScriptReferences(source) {
  const text = productionTsSource(source);
  const code = productionTsCode(source);
  const references = new Set();
  const add = (values) => values.forEach((value) => references.add(value));
  const collectModules = (pattern, keywordGroup, valueGroup) => {
    for (const match of text.matchAll(pattern)) {
      const keyword = match[keywordGroup];
      if (code.slice(match.index, match.index + keyword.length) !== keyword) continue;
      if (!match[valueGroup].includes('${')) add(referencedIn(match[valueGroup]));
    }
  };
  collectModules(/\b(import)\s*(?:\(\s*)?(['"`])([^'"`\n]+)\2/g, 1, 3);
  collectModules(/\b(import|export)\s+[^;]*?\bfrom\s*(['"`])([^'"`\n]+)\2/g, 1, 3);

  const bindings = scriptBindings(code, text);
  const functions = functionDefinitions(code, text);
  const executableParams = new Map();
  for (const fn of functions) {
    const positions = new Set();
    for (const call of callsNamed(fn.code, fn.text, JS_LAUNCHERS)) {
      for (const [index, param] of fn.params.entries()) {
        if (param && call.args.slice(0, 2).some((arg) => identifierAppears(arg.code, param))) {
          positions.add(index);
        }
      }
    }
    if (positions.size > 0) executableParams.set(fn.name, positions);
  }

  const aliases = new Map();
  for (const [binding, scriptsInValue] of bindings) {
    const iterator = new RegExp(
      `\\b${binding.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*\\.(?:map|flatMap|forEach)\\s*\\(\\s*(?:async\\s*)?\\(?\\s*([A-Za-z_$][\\w$]*)`,
      'g',
    );
    for (let match = iterator.exec(code); match !== null; match = iterator.exec(code)) {
      aliases.set(match[1], scriptsInValue);
    }
  }

  const launchCalls = callsNamed(code, text, JS_LAUNCHERS);
  for (const call of launchCalls) {
    for (const arg of call.args.slice(0, 2)) {
      add(referencedIn(arg.text));
      for (const [binding, scriptsInValue] of bindings) {
        if (identifierAppears(arg.code, binding)) add(scriptsInValue);
      }
    }
  }

  const wrapperCalls = callsNamed(code, text, new Set(executableParams.keys()));
  const definitionStarts = new Set(functions.map((fn) => fn.nameStart));
  for (const call of wrapperCalls) {
    if (definitionStarts.has(call.start)) continue;
    for (const position of executableParams.get(call.name)) {
      const arg = call.args[position];
      if (!arg) continue;
      add(referencedIn(arg.text));
      for (const [binding, scriptsInValue] of bindings) {
        if (identifierAppears(arg.code, binding)) add(scriptsInValue);
      }
      for (const [alias, scriptsInValue] of aliases) {
        if (identifierAppears(arg.code, alias)) add(scriptsInValue);
      }
    }
  }

  return [...references];
}

let workflowParserSelection;
function workflowReferences(path) {
  workflowParserSelection ??= selectWorkflowParser();
  const { parser, missing } = workflowParserSelection;
  if (!parser) {
    failures.push(
      `cannot parse ${path}: no YAML parser available (tried ${missing.join(', ')})`,
    );
    return [];
  }

  const parsed = parseWorkflowFile(parser, path);
  if (!parsed.ok) {
    const detail = parsed.diagnostic || parsed.message || `exit ${parsed.status}`;
    failures.push(`${path} is not a readable workflow: ${detail}`);
    return [];
  }
  if (!parsed.jobs || Object.keys(parsed.jobs).length === 0) {
    failures.push(`${path} parses, but declares no jobs — it cannot be an execution root`);
    return [];
  }

  const runs = [];
  for (const [job, definition] of Object.entries(parsed.jobs)) {
    const inspected = workflowJobRuns(definition);
    if (inspected.invalidSteps) {
      failures.push(`${path}: job ${job} has a steps value that is not a list`);
      continue;
    }
    for (const invalid of inspected.invalidRuns) {
      failures.push(`${path}: job ${job} step #${invalid.index + 1} has a non-string run value`);
    }
    runs.push(...inspected.runs);
  }
  return referencedIn(shellCodeOnly(runs.join('\n')));
}

function packageReferences(path) {
  let document;
  try {
    document = JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    failures.push(`${path} is not valid JSON: ${error.message}`);
    return [];
  }
  if (document.scripts === undefined) return [];
  if (!document.scripts || typeof document.scripts !== 'object' || Array.isArray(document.scripts)) {
    failures.push(`${path} has a scripts value that is not an object`);
    return [];
  }
  const commands = [];
  for (const [name, command] of Object.entries(document.scripts)) {
    if (typeof command !== 'string') {
      failures.push(`${path} script ${name} is not a string`);
      continue;
    }
    commands.push(command);
  }
  return referencedIn(shellCodeOnly(commands.join('\n')));
}

function referencesFrom(path) {
  if (path.startsWith(`${join(root, '.github', 'workflows')}${sep}`)) {
    return workflowReferences(path);
  }
  if (basename(path) === 'package.json') return packageReferences(path);

  const source = readFileSync(path, 'utf8');
  if (path.endsWith('.mjs')) return javaScriptReferences(source);
  return referencedIn(shellCodeOnly(source));
}

const reachable = new Set();
const queue = [];
function enqueue(script) {
  if (reachable.has(script)) return;
  reachable.add(script);
  queue.push(script);
}

for (const rootFile of roots) {
  for (const script of referencesFrom(rootFile)) enqueue(script);
}

while (queue.length > 0) {
  const script = queue.shift();

  // This file names the scripts in UNEXECUTED, which would otherwise launder
  // every one of them into "reachable" — an exemption list that marks its own
  // entries alive would report itself as the thing keeping them alive.
  if (script === SELF) continue;

  for (const next of referencesFrom(join(scriptsDir, script))) enqueue(next);

  // The runner does not name its checks; it globs them.
  if (script === RUNNER) {
    for (const candidate of scripts) {
      if (candidate.includes('/')) continue;
      if (globSuffixes.some((suffix) => candidate.endsWith(suffix))) enqueue(candidate);
    }
  }
}

const orphans = scripts.filter((script) => !reachable.has(script) && !UNEXECUTED.has(script));
for (const script of orphans) {
  failures.push(
    `scripts/${script} is executed by nothing: no workflow, hook, npm script or reachable script names it, ` +
      'and no runner glob covers it. Wire it up, or record why it does not run in UNEXECUTED ' +
      `in scripts/${SELF}`,
  );
}

for (const script of UNEXECUTED.keys()) {
  if (!scripts.includes(script)) {
    failures.push(
      `UNEXECUTED names scripts/${script}, which does not exist — most likely it was renamed, which would ` +
        'move the real file back into the checked set under a new name while this entry keeps covering it',
    );
  } else if (reachable.has(script)) {
    failures.push(
      `scripts/${script} is listed in UNEXECUTED but something does run it now — remove the entry so the ` +
        'exemption list keeps meaning what it says',
    );
  }
}

if (failures.length > 0) {
  console.error('❌ Script execution coverage failed:');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(
  `✅ script execution coverage: ${reachable.size}/${scripts.length} scripts reachable from ${roots.length} ` +
    `entry point(s), ${UNEXECUTED.size} exempt with a recorded reason`,
);
