/// Rune-backed auth readiness for component tests.
///
/// The real `$lib/stores/auth.svelte` readiness flag is `$state`, so a page's
/// auth `$effect` re-runs when a session is re-checked — which is how a second
/// initial load of the same singleton becomes reachable at all. A plain mocked
/// function cannot express that, so tests that need the re-run mock the store
/// against this module instead.
let ready = $state(true);

export function isAuthReady(): boolean {
	return ready;
}

export function setAuthReady(value: boolean): void {
	ready = value;
}
