// Lexical views of JavaScript sources, for the checks that assert about the
// checks themselves.
//
// Same contract as `blankRustNonCode` on the Rust side of `rust-consumer-
// contract.mjs`: one pass, and every blanked span keeps its length and its
// newlines. So `jsCodeView` and `jsTextView` are byte-aligned with the input
// and with each other — a detector can locate a token in the code-only view
// and read the literal out of the text view at the very same offset.
//
// The two views exist for the same reason their Rust twins do. Structure has to
// be read where string bodies cannot pretend to be code (`'backend.includes('`
// inside a diagnostic message is not a call), while values have to be read
// where the literals are still there (`'crates/rg-http/src/api/repos.rs'` is
// the only thing that says which language a file is written in).
//
// Regex literals are lexed, not guessed at from the outside: `/'/` opens no
// string and `/*/.test(x)` opens no comment. Telling `/` apart from division
// needs the previous token, so the scanner tracks whether the last significant
// token can end an expression — the standard rule, and the reason this cannot
// be a `replace()` chain.

const blankExceptNewlines = (span) => span.replace(/[^\n]/g, ' ');

// Words after which a `/` starts a regular expression rather than a division.
const REGEX_KEYWORDS = new Set([
  'return', 'typeof', 'instanceof', 'in', 'of', 'new', 'delete', 'void', 'do',
  'else', 'case', 'yield', 'await', 'throw',
]);

// Character classes as code-unit comparisons: the scanner asks one of these at
// nearly every character of every corpus file, and a regex test per character
// was most of its time. Each is exactly the class its comment spells.

/** `[A-Za-z_$]` */
const isIdentStart = (code) =>
  (code >= 65 && code <= 90) || (code >= 97 && code <= 122) || code === 95 || code === 36;
/** `[A-Za-z0-9_$]` */
const isIdentPart = (code) => isIdentStart(code) || (code >= 48 && code <= 57);
/** `[0-9]` */
const isDigit = (code) => code >= 48 && code <= 57;
/** `[0-9a-fA-FxXoObBn_.]` */
const isNumberPart = (code) =>
  isDigit(code)
  || (code >= 97 && code <= 102)
  || (code >= 65 && code <= 70)
  || code === 120 || code === 88 || code === 111 || code === 79
  || code === 98 || code === 66 || code === 110 || code === 95 || code === 46;
/** `\s`: the ASCII spaces by code, anything else by the regex itself. */
const isSpace = (ch) => {
  const code = ch.charCodeAt(0);
  if (code < 128) return code === 32 || (code >= 9 && code <= 13);
  return /\s/.test(ch);
};

/** End index (exclusive) of the quoted literal opening at `start`. */
function quotedEnd(source, start) {
  const quote = source[start];
  let i = start + 1;
  while (i < source.length) {
    if (source[i] === '\\') {
      i += 2;
      continue;
    }
    if (source[i] === quote) return i + 1;
    if (source[i] === '\n' && quote !== '`') return i + 1;
    i += 1;
  }
  return source.length;
}

/**
 * End index (exclusive) of the regex literal opening at `start`, or `null` when
 * the `/` does not in fact open one (unterminated, or a line comment).
 */
function regexEnd(source, start) {
  let i = start + 1;
  let inClass = false;
  while (i < source.length) {
    const ch = source[i];
    if (ch === '\\') {
      i += 2;
      continue;
    }
    if (ch === '\n') return null;
    if (inClass) {
      if (ch === ']') inClass = false;
    } else if (ch === '[') {
      inClass = true;
    } else if (ch === '/') {
      i += 1;
      while (i < source.length && isIdentPart(source.charCodeAt(i))) i += 1;
      return i;
    }
    i += 1;
  }
  return null;
}

/**
 * One lexical pass over JavaScript source that blanks the non-executable spans.
 *
 * `blankLiterals` decides whether string, template and regex bodies are blanked
 * too or copied through verbatim; comments are blanked either way. A template's
 * `${…}` substitutions are code in both views — they are scanned, not blanked,
 * so an expression written inside an interpolation is not invisible to a
 * detector reading the code view.
 */
function scanJs(source, { blankLiterals }) {
  // The view is the source with some spans blanked, so code is never appended:
  // a verbatim stretch is copied as one slice when a blank interrupts it.
  const parts = [];
  let copied = 0;
  let i = 0;
  // Whether the previous significant token can END an expression. `/` after
  // such a token is division; anywhere else it opens a regex.
  let afterValue = false;
  // Open template literals whose `${…}` we are currently inside, innermost
  // last. Each entry counts the braces opened since the substitution began, so
  // an object literal inside an interpolation cannot close it early.
  const templates = [];

  const emitBlank = (end) => {
    if (i > copied) parts.push(source.slice(copied, i));
    parts.push(blankExceptNewlines(source.slice(i, end)));
    copied = end;
    i = end;
  };
  const emitCode = (end) => {
    i = end;
  };
  const emitLiteral = (end) => {
    if (blankLiterals) emitBlank(end);
    else emitCode(end);
  };

  // The literal chunk of a template, up to `${` or the closing backtick.
  const templateChunk = (start) => {
    let j = start;
    while (j < source.length) {
      if (source[j] === '\\') {
        j += 2;
        continue;
      }
      if (source[j] === '`') return { end: j + 1, closed: true };
      if (source[j] === '$' && source[j + 1] === '{') return { end: j, closed: false };
      j += 1;
    }
    return { end: source.length, closed: true };
  };

  while (i < source.length) {
    if (source[i] === '/' && source[i + 1] === '/') {
      const nl = source.indexOf('\n', i + 2);
      emitBlank(nl < 0 ? source.length : nl);
      continue;
    }
    if (source[i] === '/' && source[i + 1] === '*') {
      const close = source.indexOf('*/', i + 2);
      emitBlank(close < 0 ? source.length : close + 2);
      continue;
    }
    if (source[i] === '"' || source[i] === "'") {
      emitLiteral(quotedEnd(source, i));
      afterValue = true;
      continue;
    }
    if (source[i] === '`') {
      // The backtick and the literal chunk after it; `${` hands control back to
      // the main loop so the substitution is scanned as code.
      const chunk = templateChunk(i + 1);
      emitLiteral(chunk.end);
      if (chunk.closed) {
        afterValue = true;
      } else {
        emitCode(i + 2); // `${`
        templates.push(0);
        afterValue = false;
      }
      continue;
    }
    if (templates.length > 0 && source[i] === '{') {
      templates[templates.length - 1] += 1;
      emitCode(i + 1);
      afterValue = false;
      continue;
    }
    if (templates.length > 0 && source[i] === '}') {
      if (templates[templates.length - 1] === 0) {
        // Closes the substitution: back into the template's literal chunk.
        templates.pop();
        emitCode(i + 1);
        const chunk = templateChunk(i);
        emitLiteral(chunk.end);
        if (chunk.closed) {
          afterValue = true;
        } else {
          emitCode(i + 2);
          templates.push(0);
          afterValue = false;
        }
        continue;
      }
      templates[templates.length - 1] -= 1;
      emitCode(i + 1);
      afterValue = false;
      continue;
    }
    if (source[i] === '/' && !afterValue) {
      const end = regexEnd(source, i);
      if (end !== null) {
        // The delimiters stay so `/…/.test(x)` still reads as a call site; only
        // the pattern body follows `blankLiterals`.
        emitCode(i + 1);
        const bodyEnd = end - 1;
        let flagEnd = bodyEnd;
        while (flagEnd > i && source[flagEnd] !== '/') flagEnd -= 1;
        emitLiteral(flagEnd);
        emitCode(end);
        afterValue = true;
        continue;
      }
    }
    if (isIdentStart(source.charCodeAt(i))) {
      let j = i + 1;
      while (j < source.length && isIdentPart(source.charCodeAt(j))) j += 1;
      const word = source.slice(i, j);
      emitCode(j);
      afterValue = !REGEX_KEYWORDS.has(word);
      continue;
    }
    if (isDigit(source.charCodeAt(i))) {
      let j = i;
      while (j < source.length && isNumberPart(source.charCodeAt(j))) j += 1;
      emitCode(j);
      afterValue = true;
      continue;
    }
    const ch = source[i];
    emitCode(i + 1);
    if (!isSpace(ch)) afterValue = ch === ')' || ch === ']';
    continue;
  }

  if (i > copied) parts.push(source.slice(copied, i));
  return parts.join('');
}

/** Comments and literal bodies blanked — the view structure is read in. */
export function jsCodeView(source) {
  return scanJs(source, { blankLiterals: true });
}

/** Comments blanked, literals intact — the view values are read from. */
export function jsTextView(source) {
  return scanJs(source, { blankLiterals: false });
}
