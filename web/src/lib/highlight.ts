/**
 * Syntax highlighting for the code view and the file editor
 * (card_61e77c8abec1).
 *
 * Two defects lived here. The blob page ran `hljs.highlightElement` on every
 * line separately, so anything spanning lines — a block comment, a multi-line
 * string — was coloured as if each line stood alone, and an unknown extension
 * was auto-detected once per line, slowly and inconsistently. And the page
 * imported `highlight.js` whole: every one of its ~190 grammars, a 912 KB
 * chunk. This module highlights a file once, as one text, and cuts the result
 * into lines; and it registers only the grammars the extension map names.
 *
 * Callers import it dynamically, so the grammars stay out of the main bundle.
 */
import hljs from 'highlight.js/lib/core';
import bash from 'highlight.js/lib/languages/bash';
import c from 'highlight.js/lib/languages/c';
import cpp from 'highlight.js/lib/languages/cpp';
import csharp from 'highlight.js/lib/languages/csharp';
import css from 'highlight.js/lib/languages/css';
import diff from 'highlight.js/lib/languages/diff';
import dockerfile from 'highlight.js/lib/languages/dockerfile';
import go from 'highlight.js/lib/languages/go';
import ini from 'highlight.js/lib/languages/ini';
import java from 'highlight.js/lib/languages/java';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import kotlin from 'highlight.js/lib/languages/kotlin';
import makefile from 'highlight.js/lib/languages/makefile';
import markdown from 'highlight.js/lib/languages/markdown';
import php from 'highlight.js/lib/languages/php';
import protobuf from 'highlight.js/lib/languages/protobuf';
import python from 'highlight.js/lib/languages/python';
import ruby from 'highlight.js/lib/languages/ruby';
import rust from 'highlight.js/lib/languages/rust';
import sql from 'highlight.js/lib/languages/sql';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';

const GRAMMARS = {
  bash,
  c,
  cpp,
  csharp,
  css,
  diff,
  dockerfile,
  go,
  ini,
  java,
  javascript,
  json,
  kotlin,
  makefile,
  markdown,
  php,
  protobuf,
  python,
  ruby,
  rust,
  sql,
  typescript,
  xml,
  yaml,
};

for (const [name, grammar] of Object.entries(GRAMMARS)) {
  hljs.registerLanguage(name, grammar);
}

/** Grammar by file extension. Every value is a key of `GRAMMARS`. */
export const LANGUAGE_BY_EXTENSION: Record<string, keyof typeof GRAMMARS> = {
  sh: 'bash',
  bash: 'bash',
  zsh: 'bash',
  c: 'c',
  h: 'c',
  cc: 'cpp',
  cpp: 'cpp',
  cxx: 'cpp',
  hh: 'cpp',
  hpp: 'cpp',
  cs: 'csharp',
  css: 'css',
  scss: 'css',
  diff: 'diff',
  patch: 'diff',
  go: 'go',
  ini: 'ini',
  toml: 'ini',
  cfg: 'ini',
  java: 'java',
  js: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  jsx: 'javascript',
  json: 'json',
  kt: 'kotlin',
  kts: 'kotlin',
  md: 'markdown',
  markdown: 'markdown',
  php: 'php',
  proto: 'protobuf',
  py: 'python',
  rb: 'ruby',
  rs: 'rust',
  sql: 'sql',
  ts: 'typescript',
  tsx: 'typescript',
  mts: 'typescript',
  svelte: 'xml',
  vue: 'xml',
  html: 'xml',
  htm: 'xml',
  xml: 'xml',
  svg: 'xml',
  yaml: 'yaml',
  yml: 'yaml',
};

/** Files known by name rather than by extension. */
const LANGUAGE_BY_NAME: Record<string, keyof typeof GRAMMARS> = {
  dockerfile: 'dockerfile',
  containerfile: 'dockerfile',
  makefile: 'makefile',
  gnumakefile: 'makefile',
  'cargo.lock': 'ini',
};

/** The grammar for `path`, or `''` when there is none: no guessing. */
export function languageForPath(path: string): string {
  const name = path.split('/').pop()?.toLowerCase() ?? '';
  if (LANGUAGE_BY_NAME[name]) return LANGUAGE_BY_NAME[name];
  const dot = name.lastIndexOf('.');
  if (dot <= 0) return '';
  return LANGUAGE_BY_EXTENSION[name.slice(dot + 1)] ?? '';
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/**
 * `content` as highlighted HTML, the whole text at once. Without a known
 * grammar the text is escaped and left uncoloured — auto-detection guessed
 * differently from one file to the next and cost the most on the largest.
 */
export function highlightSource(content: string, language: string): string {
  if (language && hljs.getLanguage(language)) {
    return hljs.highlight(content, { language, ignoreIllegals: true }).value;
  }
  return escapeHtml(content);
}

/**
 * Cut highlighted HTML into one well-formed fragment per source line.
 *
 * highlight.js emits only `<span …>`, `</span>` and escaped text, and a span
 * may run across a newline — a block comment does. At each newline every open
 * span is closed, and reopened at the start of the next line, so each line
 * renders on its own with the colour it has inside the whole file.
 */
export function splitHighlightedLines(html: string): string[] {
  const lines: string[] = [];
  const open: string[] = [];
  let current = '';
  for (const match of html.matchAll(/(<span[^>]*>)|(<\/span>)|([^<]+)/g)) {
    const [, opening, closing, text] = match;
    if (opening) {
      open.push(opening);
      current += opening;
    } else if (closing) {
      open.pop();
      current += closing;
    } else if (text) {
      const parts = text.split('\n');
      current += parts[0];
      for (const part of parts.slice(1)) {
        current += '</span>'.repeat(open.length);
        lines.push(current);
        current = open.join('') + part;
      }
    }
  }
  lines.push(current);
  return lines;
}

/** `content` highlighted as a whole and cut into lines. */
export function highlightLines(content: string, language: string): string[] {
  return splitHighlightedLines(highlightSource(content, language));
}
