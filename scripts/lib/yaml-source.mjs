// Helpers for asserting against YAML documents from the contract checks.
//
// The Rust and TypeScript halves of this repository each got a production view
// — `scripts/lib/rust-source.mjs`, `scripts/lib/ts-source.mjs` — for one
// reason: raw bytes are not the program, so a construct that was merely
// commented out still satisfies `source.includes('…')` and the gate stays green
// over code nothing runs. YAML was the third language with the same defect and
// no such view.
//
// Proved on `.github/workflows/regression.yml`: commenting out the single line
// `run: node scripts/run-contract-checks.mjs` — the step that executes EVERY
// contract check in this repository — left `local-gate-coverage-contract-
// check.mjs`, `workflow-yaml-contract-check.mjs`, `deploy-config-concurrency-
// contract-check.mjs` and `markdown-sanitizer-contract-check.mjs` all green
// (card_fad8ad0ef007). The coverage gate whose entire job is "every check
// mechanism is executed by a job of regression.yml" had itself become the kind
// of comment it exists to catch.
//
// What a *production view* of YAML is, stated exactly, because the answer
// decides where the blanking stops: it is the source minus what a parser
// discards. A parser discards comments. It does NOT discard block-scalar
// content — the body of a `run: |` step is document data, and a `#` line inside
// it is a *shell* comment, a different language's problem with its own guard
// (`activeShell` in `local-gate-coverage-contract-check.mjs`). So comments are
// blanked outside block scalars and left alone inside them, and the view says
// what a parser would say.
//
// Where a claim is structural — "this job still runs this command" — the parsed
// document is the right subject and `scripts/lib/workflow.mjs` is the boundary
// for it. This file is for the claims that are genuinely textual: a value that
// no parser exposes, or a marker comment that a parser deliberately throws away
// (`deploy/docker-compose.yml` marks its ForgeKeep HTTP mapping with `# HTTP`).
//
// Every view here is byte-aligned with its input, so an offset found in one
// addresses the same character in the other.

const blankExceptNewlines = (span) => span.replace(/[^\n]/g, ' ');

// A block scalar opens with `|` or `>` as the whole value, optionally carrying
// an indentation indicator and a chomping indicator. It has to be the whole
// value: `expr: a > b` is a plain scalar that merely contains `>`, and reading
// it as a block header would swallow every following line — comments included —
// out of the view, which is the false-green direction.
const BLOCK_SCALAR_HEADER = /(?::|^[ \t]*-)[ \t]+[|>][0-9]*[+-]?[0-9]*[ \t]*$/;

// Characters after which a quote opens a quoted scalar rather than sitting
// inside a plain one. Without this, the apostrophe in `run: it's fine # note`
// would open a single-quoted scalar that runs to the end of the line and hides
// the comment from the view — raw bytes again, by a subtler route.
const SCALAR_OPENERS = ':-,[{?';

/**
 * Index of the `#` that opens this line's comment, or `-1`.
 *
 * A `#` is a comment opener only at the start of a line, or preceded by a space
 * or tab — `image: prom/prometheus:v3#1` carries no comment — and never inside
 * a quoted scalar.
 */
function commentStart(line) {
  let previous = null;
  let i = 0;
  while (i < line.length) {
    const ch = line[i];

    if ((ch === "'" || ch === '"') && (previous === null || SCALAR_OPENERS.includes(previous))) {
      const quote = ch;
      i += 1;
      while (i < line.length) {
        if (quote === '"' && line[i] === '\\') {
          i += 2;
          continue;
        }
        if (line[i] === quote) {
          // `''` inside a single-quoted scalar is an escaped quote, not the end.
          if (quote === "'" && line[i + 1] === "'") {
            i += 2;
            continue;
          }
          i += 1;
          break;
        }
        i += 1;
      }
      previous = quote;
      continue;
    }

    if (ch === '#' && (i === 0 || line[i - 1] === ' ' || line[i - 1] === '\t')) return i;
    if (ch !== ' ' && ch !== '\t') previous = ch;
    i += 1;
  }
  return -1;
}

/**
 * Where every comment of `source` sits: its line index, its column, and the
 * `[start, end)` offsets of its body — the text after the `#`.
 *
 * Block-scalar bodies are skipped whole: their `#` lines are content a parser
 * hands to whatever consumes the value, not YAML comments.
 */
function commentRanges(source) {
  const ranges = [];
  let offset = 0;
  let block = null;
  let index = 0;

  for (const line of source.split('\n')) {
    const start = offset;
    offset += line.length + 1;
    const lineIndex = index;
    index += 1;
    const indent = line.length - line.trimStart().length;

    if (block !== null) {
      // A blank line belongs to the block whatever its indentation; anything
      // indented no further than the header closes it.
      if (line.trim() === '' || indent > block.indent) continue;
      block = null;
    }

    const at = commentStart(line);
    // The `#` itself is deliberately outside the range. Blanking it too would
    // turn a whole-line comment into a run of spaces, and a run of spaces
    // indented deeper than an open block scalar is *content* to a parser — so
    // the view would stop being the same document the file is. Keeping the
    // marker leaves the view parseable into exactly the original document,
    // which `yaml-source-parser-contract-check.mjs` asserts over every `.yml`
    // this repository ships.
    if (at >= 0) ranges.push({ line: lineIndex, column: at, start: start + at + 1, end: start + line.length });

    const code = at < 0 ? line : line.slice(0, at);
    if (BLOCK_SCALAR_HEADER.test(code)) block = { indent };
  }

  return ranges;
}

/**
 * The comment-free view of a YAML document, byte-aligned with `source`.
 *
 * This is the view a textual assertion about YAML must anchor in. A raw grep
 * for `run: npm test` is satisfied by `# run: npm test`, so the gate reports a
 * step CI no longer has; here that line reads as deleted, which is what it is.
 *
 * Comment *bodies* are blanked; the `#` that opens them stays. The view is
 * therefore still the same document to a parser — which is the property the
 * fixture checks against every `.yml` in the tree — while carrying none of the
 * text an assertion could match.
 */
export function productionYamlSource(source) {
  let out = source;
  for (const { start, end } of commentRanges(source)) {
    out = out.slice(0, start) + blankExceptNewlines(out.slice(start, end)) + out.slice(end);
  }
  return out;
}

/**
 * Each line of `source` split into its code half and its comment, for the one
 * kind of claim a production view cannot express: an assertion *about* a marker
 * comment.
 *
 * `deploy/docker-compose.yml` marks the ForgeKeep HTTP port mapping with a
 * trailing `# HTTP`, because a parser discards comments and the value alone
 * cannot say which service owns it. Reading that with a regex over the raw
 * bytes works until a `#` appears inside a quoted scalar; splitting the line
 * the way the parser would means the marker and the value are read from the
 * halves they actually live in.
 *
 * `comment` is the comment text with its `#` and surrounding whitespace
 * stripped, or `null` when the line carries none. `code` is the line up to the
 * `#`, with any comment further left — there is none — already blanked.
 */
export function yamlAnnotatedLines(source) {
  const codeLines = productionYamlSource(source).split('\n');
  const columns = new Map();
  for (const { line, column } of commentRanges(source)) columns.set(line, column);

  return source.split('\n').map((raw, index) => {
    const code = codeLines[index] ?? raw;
    const at = columns.get(index);
    return {
      code: at === undefined ? code : code.slice(0, at),
      comment: at === undefined ? null : raw.slice(at + 1).trim(),
    };
  });
}
