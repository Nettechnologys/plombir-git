/// Search utility functions.

/// Highlight all occurrences of `query` in `text` by wrapping them
/// in `<mark>` tags.  Returns HTML-safe (escaped) output.
/// If `query` is empty or `text` is empty, returns the escaped text.
export function highlightText(text: string, query: string): string {
  if (!text || !query) return escapeHtml(text);
  
  const escaped = escapeHtml(text);
  const words = query.trim().split(/\s+/).filter(Boolean);
  if (words.length === 0) return escaped;

  // Escape the query words for use in regex
  const patterns = words.map(w => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
  const regex = new RegExp(`(${patterns.join('|')})`, 'gi');
  
  return escaped.replace(regex, '<mark class="search-highlight">$1</mark>');
}

function escapeHtml(str: string): string {
  return str
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}
