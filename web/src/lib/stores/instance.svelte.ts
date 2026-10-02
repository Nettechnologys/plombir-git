/// Global instance state store — maintenance mode, banner, keyboard shortcuts.
/// Uses Svelte 5 runes ($state).

// ── Instance Banner ─────────────────────────────────────

let bannerMessage = $state('');
let bannerType = $state<'info' | 'warning' | 'error'>('info');

export function getBanner() {
  return { message: bannerMessage, type: bannerType };
}

export function setBanner(message: string, type: 'info' | 'warning' | 'error' = 'info') {
  bannerMessage = message;
  bannerType = type;
}

export function clearBanner() {
  bannerMessage = '';
}

// ── Source code link ────────────────────────────────────

/// Where this instance's source is offered (AGPL §13), as `GET /api/v1/instance`
/// reported it. `null` until that answer arrives — and for good if it never
/// does: a link the server did not supply would be a guess, and the only guess
/// available is upstream's repository, which is exactly the wrong one for a
/// modified fork.
export interface SourceLink {
  url: string;
  commit: string | null;
}

let sourceLink = $state<SourceLink | null>(null);

export function getSourceLink(): SourceLink | null {
  return sourceLink;
}

export function setSourceLink(link: SourceLink | null) {
  sourceLink = link;
}

// ── Keyboard Shortcuts ──────────────────────────────────

/// Call this once in root layout to register global keyboard shortcuts.
export function registerKeyboardShortcuts() {
  if (typeof window === 'undefined') return;
  
  function handler(e: KeyboardEvent) {
    // Don't trigger when typing in input/textarea
    const tag = (e.target as HTMLElement)?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
    
    // '?' — Focus global search
    if (e.key === '?' && !e.shiftKey) {
      e.preventDefault();
      focusSearch();
    }
    // 'g' then 'i' — Go to Issues
    // 'g' then 'p' — Go to Pull Requests
    // These are handled by the navigate shortcut system
  }

  document.addEventListener('keydown', handler);
  return () => document.removeEventListener('keydown', handler);
}

function focusSearch() {
  // Try to find and focus the global search input
  const searchInput = document.querySelector<HTMLInputElement>(
    '[data-global-search], input[type="search"]'
  );
  if (searchInput) {
    searchInput.focus();
    searchInput.select();
  }
}
