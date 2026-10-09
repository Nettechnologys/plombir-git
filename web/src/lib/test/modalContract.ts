import { expect } from 'vitest';

import { click, settle } from './render';

// Shared acceptance for every page that shows a `Modal` (card_4a99471945dc):
// the dialog opens with focus inside, a click inside it and Space/Enter typed
// into it leave it open and leave the keystroke to the browser, and Escape
// closes it and gives focus back to the control that opened it.

export function openDialog(container: ParentNode): HTMLElement | null {
	return container.querySelector<HTMLElement>('[role="dialog"]');
}

async function keydown(target: Element, key: string): Promise<KeyboardEvent> {
	const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true });
	target.dispatchEvent(event);
	await settle();
	return event;
}

export async function openModalFrom(container: ParentNode, opener: HTMLElement): Promise<HTMLElement> {
	opener.focus();
	expect(document.activeElement).toBe(opener);
	await click(opener);
	const dialog = openDialog(container);
	expect(dialog, 'the opener did not show a dialog').not.toBeNull();
	expect(dialog!.getAttribute('aria-modal')).toBe('true');
	const labelledby = dialog!.getAttribute('aria-labelledby');
	expect(labelledby, 'the dialog is not named by a heading').toBeTruthy();
	expect(document.getElementById(labelledby!)?.textContent?.trim()).toBeTruthy();
	expect(dialog!.contains(document.activeElement), 'focus did not move into the dialog').toBe(true);
	return dialog!;
}

/**
 * `field` is the input the user types into; for a confirmation without one,
 * pass the dialog's heading — the old overlay closed on Space from anywhere.
 */
export async function expectModalSurvivesInteraction(
	container: ParentNode,
	field: HTMLElement,
): Promise<void> {
	await click(field);
	expect(openDialog(container), 'a click inside the dialog closed it').not.toBeNull();

	field.focus();
	const space = await keydown(field, ' ');
	expect(space.defaultPrevented, 'Space typed inside the dialog was swallowed').toBe(false);
	const enter = await keydown(field, 'Enter');
	expect(enter.defaultPrevented, 'Enter typed inside the dialog was swallowed').toBe(false);
	expect(openDialog(container), 'Space or Enter inside the dialog closed it').not.toBeNull();
}

export async function expectEscapeClosesAndRestoresFocus(
	container: ParentNode,
	opener: HTMLElement,
): Promise<void> {
	const dialog = openDialog(container)!;
	const from = dialog.contains(document.activeElement) ? (document.activeElement as HTMLElement) : dialog;
	const escape = await keydown(from, 'Escape');
	expect(escape.defaultPrevented).toBe(true);
	expect(openDialog(container), 'Escape did not close the dialog').toBeNull();
	expect(document.activeElement, 'focus did not return to the opener').toBe(opener);
}
