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

// ── Self-service registration ───────────────────────────

/// Whether `/users/register` would accept a sign-up, as `GET /api/v1/instance`
/// reported it (card_e1baa94866ed). `null` until that answer arrives; only an
/// explicit `false` hides the sign-up links, so a server that predates the
/// field keeps them.
let registrationOpen = $state<boolean | null>(null);

export function getRegistrationOpen(): boolean | null {
  return registrationOpen;
}

export function setRegistrationOpen(open: boolean | null) {
  registrationOpen = open;
}

// ── Keyboard Shortcuts ──────────────────────────────────

/// Call this once in root layout to register global keyboard shortcuts.
///
/// One shortcut: `?` focuses the global search. It is matched on the
/// character, not on the modifier — `?` takes Shift on most layouts, and the
/// old `!e.shiftKey` guard made it unreachable on all of them
/// (card_c30077df5603).
export function registerKeyboardShortcuts() {
  if (typeof window === 'undefined') return;

  function handler(e: KeyboardEvent) {
    // Not while typing, and not when another modifier makes it a different chord.
    const target = e.target as HTMLElement | null;
    const tag = target?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || target?.isContentEditable) return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;

    if (e.key === '?') {
      e.preventDefault();
      focusSearch();
    }
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
