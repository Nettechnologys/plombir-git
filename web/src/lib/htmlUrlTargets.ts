import { tick } from 'svelte';

// What the pages do with the addresses the API hands out as `html_url`
// (card_dca6f5ecbc87). The server holds each template to the page it names —
// `every_html_url_names_a_page_the_web_app_serves` in `rg-http` — so a page
// that stops honouring its fragment or query parameter fails that test.

/**
 * The id a `?pipeline=<id>` / `?release=<id>` parameter names: a positive
 * integer, or `null` for anything else.
 */
export function linkedId(raw: string | null): number | null {
  if (raw === null || !/^[1-9][0-9]*$/.test(raw)) return null;
  const id = Number(raw);
  return Number.isSafeInteger(id) ? id : null;
}

/**
 * Scroll to the element the URL fragment names, once the page has rendered it.
 *
 * A comment's `html_url` is `…/issues/3#comment-41`, and a browser only scrolls
 * to a fragment that exists when the document loads — a comment fetched
 * afterwards is never reached. A page calls this after its data has arrived.
 */
export async function revealHash(): Promise<void> {
  if (typeof window === 'undefined') return;
  let id: string;
  try {
    id = decodeURIComponent(window.location.hash.slice(1));
  } catch {
    return;
  }
  if (!id) return;
  await tick();
  document.getElementById(id)?.scrollIntoView?.({ block: 'start' });
}
