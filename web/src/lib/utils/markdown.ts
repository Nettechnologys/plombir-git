import { marked } from 'marked';

const ALLOWED_TAGS = new Set([
  'a',
  'blockquote',
  'br',
  'code',
  'del',
  'em',
  'h1',
  'h2',
  'h3',
  'h4',
  'h5',
  'h6',
  'hr',
  'img',
  'li',
  'ol',
  'p',
  'pre',
  'strong',
  'table',
  'tbody',
  'td',
  'th',
  'thead',
  'tr',
  'ul',
]);

const GLOBAL_ATTRIBUTES = new Set(['title']);
const ATTRIBUTES_BY_TAG: Record<string, Set<string>> = {
  a: new Set(['href', 'title']),
  code: new Set(['class']),
  img: new Set(['alt', 'src', 'title']),
  td: new Set(['align']),
  th: new Set(['align']),
};

function isSafeClassName(value: string): boolean {
  return /^[a-z0-9_:\-\s]+$/i.test(value);
}

function isSafeUrl(value: string): boolean {
  const trimmed = value.trim();
  if (!trimmed) return false;
  if (trimmed.startsWith('#') || trimmed.startsWith('/') || trimmed.startsWith('./') || trimmed.startsWith('../')) {
    return true;
  }

  try {
    const url = new URL(trimmed, 'https://plombir-git.local');
    return ['http:', 'https:', 'mailto:'].includes(url.protocol);
  } catch {
    return false;
  }
}

function isAllowedAttribute(tagName: string, attrName: string, value: string): boolean {
  if (attrName.startsWith('on')) return false;
  if (GLOBAL_ATTRIBUTES.has(attrName)) return true;
  if (!ATTRIBUTES_BY_TAG[tagName]?.has(attrName)) return false;
  if ((attrName === 'href' || attrName === 'src') && !isSafeUrl(value)) return false;
  if (attrName === 'class' && !isSafeClassName(value)) return false;
  if (attrName === 'align' && !['left', 'center', 'right'].includes(value.toLowerCase())) return false;
  return true;
}

function unwrapElement(element: Element) {
  const parent = element.parentNode;
  if (!parent) return;
  while (element.firstChild) {
    parent.insertBefore(element.firstChild, element);
  }
  parent.removeChild(element);
}

function sanitizeElement(element: Element) {
  const tagName = element.tagName.toLowerCase();
  if (!ALLOWED_TAGS.has(tagName)) {
    unwrapElement(element);
    return;
  }

  for (const attr of Array.from(element.attributes)) {
    const attrName = attr.name.toLowerCase();
    if (!isAllowedAttribute(tagName, attrName, attr.value)) {
      element.removeAttribute(attr.name);
    }
  }

  if (tagName === 'a') {
    element.setAttribute('rel', 'nofollow noopener noreferrer');
  }
}

function sanitizePass(html: string): string {
  const parser = new DOMParser();
  const doc = parser.parseFromString(html, 'text/html');
  const walker = doc.createTreeWalker(doc.body, NodeFilter.SHOW_ELEMENT);
  const elements: Element[] = [];

  while (walker.nextNode()) {
    elements.push(walker.currentNode as Element);
  }

  for (const element of elements) {
    sanitizeElement(element);
  }

  return doc.body.innerHTML;
}

// One pass is parse -> sanitize -> serialize, and the caller hands the result to
// `{@html}`, which parses it a second time. Payloads that survive a sanitizer
// exploit exactly that gap: markup which means one thing to the first parse and
// another to the second (mXSS — typically via `<svg>`/`<math>` foreign content
// being unwrapped into HTML context). Re-running the sanitizer over its own
// output until it stops changing closes the gap by construction: what we return
// is a fixpoint, so the browser's final parse sees markup that has already been
// sanitized in exactly that form.
const MAX_SANITIZE_PASSES = 4;

export function sanitizeHtml(html: string): string {
  if (typeof DOMParser === 'undefined' || typeof NodeFilter === 'undefined') {
    // Deliberately no regex fallback. There used to be one, and it was the only
    // branch the CI gate ever exercised (the app is browser-only: `ssr = false`
    // and `prerender = false` in src/routes/+layout.ts) — it shipped a
    // `javascript&#58;` bypass, because it validated attribute values before
    // HTML entities were decoded. Two implementations of one security
    // boundary means the weaker one is the one that eventually runs. If server
    // rendering is ever turned on, give this code a real DOM; do not reintroduce
    // a second sanitizer.
    throw new Error(
      'sanitizeHtml requires a DOM (DOMParser + NodeFilter); see src/lib/utils/markdown.ts'
    );
  }

  let current = sanitizePass(html);
  for (let pass = 1; pass < MAX_SANITIZE_PASSES; pass += 1) {
    const next = sanitizePass(current);
    if (next === current) return current;
    current = next;
  }

  // Markup that still re-interprets itself after four passes is not content we
  // can render safely. Fail closed rather than hand the browser something whose
  // meaning we could not pin down.
  return '';
}

export function renderMarkdown(content: string): string {
  const html = marked.parse(content || '', { async: false }) as string;
  return sanitizeHtml(html);
}
