const SHELL_WORD_BREAKS = new Set([';', '&', '|', '(', ')', '<', '>', '{', '}']);

/**
 * Return a byte-aligned shell source view with real comments blanked.
 *
 * A `#` starts a shell comment only at the beginning of a word. Hashes inside
 * quotes, parameter expansion, escaped words, and assignment values are data.
 * Keeping every non-comment byte in place lets callers use source offsets while
 * preventing prose after a live command from satisfying an executable claim.
 */
export function shellCodeOnly(source) {
  let view = '';
  let quote = null;
  let parameterDepth = 0;
  let atWordStart = true;

  for (let index = 0; index < source.length; index += 1) {
    const char = source[index];

    if (quote === "'") {
      view += char;
      if (char === "'") quote = null;
      continue;
    }

    if (quote === '"') {
      view += char;
      if (char === '\\' && index + 1 < source.length) {
        view += source[index + 1];
        index += 1;
      } else if (char === '"') {
        quote = null;
      }
      continue;
    }

    if (parameterDepth > 0) {
      view += char;
      if (char === '\\' && index + 1 < source.length) {
        view += source[index + 1];
        index += 1;
      } else if (char === "'" || char === '"') {
        quote = char;
      } else if (char === '$' && source[index + 1] === '{') {
        view += '{';
        index += 1;
        parameterDepth += 1;
      } else if (char === '}') {
        parameterDepth -= 1;
      }
      continue;
    }

    if (char === '\\' && index + 1 < source.length) {
      view += char + source[index + 1];
      index += 1;
      atWordStart = false;
      continue;
    }

    if (char === "'" || char === '"') {
      view += char;
      quote = char;
      atWordStart = false;
      continue;
    }

    if (char === '$' && source[index + 1] === '{') {
      view += '${';
      index += 1;
      parameterDepth = 1;
      atWordStart = false;
      continue;
    }

    if (char === '#' && atWordStart) {
      while (index < source.length && source[index] !== '\n') {
        view += ' ';
        index += 1;
      }
      if (index < source.length) {
        view += '\n';
        atWordStart = true;
      }
      continue;
    }

    view += char;
    if (char === '\n' || /\s/.test(char) || SHELL_WORD_BREAKS.has(char)) {
      atWordStart = true;
    } else {
      atWordStart = false;
    }
  }

  return view;
}

function shellCommands(source) {
  const commands = [];
  let command = [];
  let word = '';
  let wordStarted = false;
  let quote = null;
  let parameterDepth = 0;

  function finishWord() {
    if (wordStarted) command.push(word);
    word = '';
    wordStarted = false;
  }

  function finishCommand() {
    finishWord();
    if (command.length > 0) commands.push(command);
    command = [];
  }

  const code = shellCodeOnly(source);
  for (let index = 0; index < code.length; index += 1) {
    const char = code[index];

    if (quote === "'") {
      if (char === "'") quote = null;
      else word += char;
      continue;
    }

    if (quote === '"') {
      if (char === '\\' && index + 1 < code.length) {
        word += code[index + 1];
        index += 1;
      } else if (char === '"') {
        quote = null;
      } else {
        word += char;
      }
      continue;
    }

    if (parameterDepth > 0) {
      word += char;
      if (char === '\\' && index + 1 < code.length) {
        word += code[index + 1];
        index += 1;
      } else if (char === "'" || char === '"') {
        quote = char;
      } else if (char === '$' && code[index + 1] === '{') {
        word += '{';
        index += 1;
        parameterDepth += 1;
      } else if (char === '}') {
        parameterDepth -= 1;
      }
      continue;
    }

    if (char === '\\' && index + 1 < code.length) {
      wordStarted = true;
      word += code[index + 1];
      index += 1;
      continue;
    }

    if (char === "'" || char === '"') {
      wordStarted = true;
      quote = char;
      continue;
    }

    if (char === '$' && code[index + 1] === '{') {
      wordStarted = true;
      word += '${';
      index += 1;
      parameterDepth = 1;
      continue;
    }

    if (char === '\n' || char === ';' || char === '&' || char === '|') {
      finishCommand();
      continue;
    }

    if (/\s/.test(char) || SHELL_WORD_BREAKS.has(char)) {
      finishWord();
      continue;
    }

    wordStarted = true;
    word += char;
  }

  finishCommand();
  return commands;
}

/** True when `source` invokes the literal command prefix, not merely names it. */
export function shellInvokes(source, expectedCommand) {
  const expected = expectedCommand.trim().split(/\s+/);
  if (expected.length === 0 || expected[0] === '') return false;

  return shellCommands(source).some((words) => {
    let start = 0;
    while (start < words.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(words[start])) start += 1;
    return expected.every((word, offset) => words[start + offset] === word);
  });
}
