export type DiffLineType = 'same' | 'add' | 'del';

export interface DiffLine {
  type: DiffLineType;
  oldNumber?: number;
  newNumber?: number;
  text: string;
}

const MAX_DETAILED_DIFF_CELLS = 90000;

function splitLines(text: string): string[] {
  if (!text) return [];
  return text.split('\n');
}

function fallbackDiff(oldLines: string[], newLines: string[]): DiffLine[] {
  return [
    ...oldLines.map((text, index) => ({ type: 'del' as const, oldNumber: index + 1, text })),
    ...newLines.map((text, index) => ({ type: 'add' as const, newNumber: index + 1, text })),
  ];
}

export function buildLineDiff(oldText: string, newText: string): DiffLine[] {
  const oldLines = splitLines(oldText);
  const newLines = splitLines(newText);

  if (oldText === newText) {
    return oldLines.map((text, index) => ({
      type: 'same',
      oldNumber: index + 1,
      newNumber: index + 1,
      text,
    }));
  }

  if (oldLines.length * newLines.length > MAX_DETAILED_DIFF_CELLS) {
    return fallbackDiff(oldLines, newLines);
  }

  const rows = oldLines.length + 1;
  const cols = newLines.length + 1;
  const table = Array.from({ length: rows }, () => new Uint16Array(cols));

  for (let i = oldLines.length - 1; i >= 0; i -= 1) {
    for (let j = newLines.length - 1; j >= 0; j -= 1) {
      table[i][j] =
        oldLines[i] === newLines[j]
          ? table[i + 1][j + 1] + 1
          : Math.max(table[i + 1][j], table[i][j + 1]);
    }
  }

  const diff: DiffLine[] = [];
  let i = 0;
  let j = 0;

  while (i < oldLines.length && j < newLines.length) {
    if (oldLines[i] === newLines[j]) {
      diff.push({ type: 'same', oldNumber: i + 1, newNumber: j + 1, text: oldLines[i] });
      i += 1;
      j += 1;
    } else if (table[i + 1][j] >= table[i][j + 1]) {
      diff.push({ type: 'del', oldNumber: i + 1, text: oldLines[i] });
      i += 1;
    } else {
      diff.push({ type: 'add', newNumber: j + 1, text: newLines[j] });
      j += 1;
    }
  }

  while (i < oldLines.length) {
    diff.push({ type: 'del', oldNumber: i + 1, text: oldLines[i] });
    i += 1;
  }

  while (j < newLines.length) {
    diff.push({ type: 'add', newNumber: j + 1, text: newLines[j] });
    j += 1;
  }

  return diff;
}
