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
    const url = new URL(trimmed, 'https://ironforge.local');
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

function sanitizeWithDomParser(html: string): string {
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

function stripDangerousAttributes(tag: string): string {
  const tagName = tag.match(/^<\/?\s*([a-z0-9-]+)/i)?.[1]?.toLowerCase();
  if (!tagName || !ALLOWED_TAGS.has(tagName)) return '';

  return tag
    .replace(/\s+on[a-z]+\s*=\s*"[^"]*"/gi, '')
    .replace(/\s+on[a-z]+\s*=\s*'[^']*'/gi, '')
    .replace(/\s+on[a-z]+\s*=\s*[^\s>]+/gi, '')
    .replace(/\s+(href|src)\s*=\s*"([^"]*)"/gi, (_match, attr, value) =>
      isSafeUrl(value) ? ` ${attr}="${value}"` : ''
    )
    .replace(/\s+(href|src)\s*=\s*'([^']*)'/gi, (_match, attr, value) =>
      isSafeUrl(value) ? ` ${attr}="${value}"` : ''
    )
    .replace(/\s+(href|src)\s*=\s*([^\s>"']+)/gi, (_match, attr, value) =>
      isSafeUrl(value) ? ` ${attr}="${value}"` : ''
    )
    .replace(/\s+style\s*=\s*"[^"]*"/gi, '')
    .replace(/\s+style\s*=\s*'[^']*'/gi, '')
    .replace(/\s+style\s*=\s*[^\s>]+/gi, '');
}

export function sanitizeHtml(html: string): string {
  if (typeof DOMParser !== 'undefined' && typeof NodeFilter !== 'undefined') {
    return sanitizeWithDomParser(html);
  }

  return html
    .replace(/<script[\s\S]*?>[\s\S]*?<\/script>/gi, '')
    .replace(/<style[\s\S]*?>[\s\S]*?<\/style>/gi, '')
    .replace(/<\/?(iframe|object|embed|link|meta|base|svg|math)[^>]*>/gi, '')
    .replace(/<[^>]+>/g, stripDangerousAttributes);
}

export function renderMarkdown(content: string): string {
  const html = marked.parse(content || '', { async: false }) as string;
  return sanitizeHtml(html);
}
