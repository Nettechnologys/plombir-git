// The page-owned replacement for `window.confirm()` (card_4c186d530f59).
//
// A native dialog can only show a string, is styled by the browser, and a
// browser that suppresses dialogs answers `false` without showing anything,
// so the button looks dead. A page instead creates one `Confirmer`, renders
// `<ConfirmModal {confirmer} />` once, and awaits `confirmer.ask(...)` where it
// used to call `confirm(...)`.

export interface ConfirmRequest {
	/** Heading of the dialog: the action, e.g. "Delete SSH key". */
	title: string;
	/** Body: what happens, naming the object it happens to. */
	message: string;
	/** Label of the button that performs the action. */
	confirmLabel: string;
	/** `danger` (default) for irreversible actions, `primary` otherwise. */
	tone?: 'danger' | 'primary';
}

interface PendingConfirm extends ConfirmRequest {
	resolve: (confirmed: boolean) => void;
}

export interface Confirmer {
	readonly pending: Readonly<ConfirmRequest> | null;
	/** Resolves `true` on the action button, `false` on Cancel, Escape or the backdrop. */
	ask(request: ConfirmRequest): Promise<boolean>;
	answer(confirmed: boolean): void;
}

export function createConfirmer(): Confirmer {
	let pending = $state<PendingConfirm | null>(null);

	function answer(confirmed: boolean) {
		const current = pending;
		pending = null;
		current?.resolve(confirmed);
	}

	return {
		get pending() {
			return pending;
		},
		ask(request) {
			// A second question replaces the first, which counts as declined:
			// its caller must not act on an answer the user never gave.
			answer(false);
			return new Promise<boolean>((resolve) => {
				pending = { ...request, resolve };
			});
		},
		answer,
	};
}
