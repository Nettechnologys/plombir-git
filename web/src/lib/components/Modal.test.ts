import { afterEach, describe, expect, it, vi } from 'vitest';

import ModalHarness from '../test/ModalHarness.svelte';
import { click, element, renderComponent, settle, type RenderedComponent } from '../test/render';

// card_4a99471945dc: every modal was an overlay with `onclick={close}` around
// the dialog and a keydown that closed it on Escape, Enter and Space. These
// pin the contract `Modal.svelte` now owns for every page.

let rendered: RenderedComponent | undefined;

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

async function openHarness(props: Record<string, unknown> = {}) {
	rendered = await renderComponent(ModalHarness, props);
	const opener = element<HTMLButtonElement>(rendered.container, '#opener');
	opener.focus();
	await click(opener);
	return { container: rendered.container, opener };
}

function dialog(container: ParentNode): HTMLElement | null {
	return container.querySelector<HTMLElement>('[role="dialog"]');
}

async function key(target: Element, keyName: string, init: KeyboardEventInit = {}): Promise<KeyboardEvent> {
	const event = new KeyboardEvent('keydown', { key: keyName, bubbles: true, cancelable: true, ...init });
	target.dispatchEvent(event);
	await settle();
	return event;
}

describe('Modal', () => {
	it('is an aria-modal dialog named by its heading', async () => {
		const { container } = await openHarness();
		const panel = dialog(container)!;
		expect(panel).not.toBeNull();
		expect(panel.getAttribute('aria-modal')).toBe('true');
		expect(panel.getAttribute('aria-labelledby')).toBe('harness-title');
		expect(document.getElementById('harness-title')?.textContent).toBe('Harness');
	});

	it('uses the aria-label when there is no heading to point at', async () => {
		const { container } = await openHarness({ empty: true });
		const panel = dialog(container)!;
		expect(panel.getAttribute('aria-labelledby')).toBeNull();
		expect(panel.getAttribute('aria-label')).toBe('Empty dialog');
	});

	it('stays open on a click inside and on Space or Enter typed into its input', async () => {
		const onclose = vi.fn();
		const { container } = await openHarness({ onclose });
		const input = element<HTMLInputElement>(container, '#harness-name');

		await click(input);
		await click(dialog(container)!);
		const space = await key(input, ' ');
		const enter = await key(input, 'Enter');

		expect(space.defaultPrevented).toBe(false);
		expect(enter.defaultPrevented).toBe(false);
		expect(onclose).not.toHaveBeenCalled();
		expect(dialog(container)).not.toBeNull();
	});

	it('closes on a backdrop click', async () => {
		const onclose = vi.fn();
		const { container } = await openHarness({ onclose });
		await click(element(container, '.modal-backdrop'));
		expect(onclose).toHaveBeenCalledTimes(1);
		expect(dialog(container)).toBeNull();
	});

	it('moves focus inside on open and returns it to the opener on Escape', async () => {
		const onclose = vi.fn();
		const { container, opener } = await openHarness({ onclose });
		const input = element<HTMLInputElement>(container, '#harness-name');
		expect(document.activeElement).toBe(input);

		const escape = await key(input, 'Escape');
		expect(escape.defaultPrevented).toBe(true);
		expect(onclose).toHaveBeenCalledTimes(1);
		expect(dialog(container)).toBeNull();
		expect(document.activeElement).toBe(opener);
	});

	it('focuses the data-autofocus control first', async () => {
		const { container } = await openHarness({ autofocusCancel: true });
		expect(document.activeElement).toBe(element(container, '#harness-cancel'));
	});

	it('falls back to the dialog itself when nothing inside can take focus', async () => {
		const { container } = await openHarness({ empty: true });
		expect(document.activeElement).toBe(dialog(container));
	});

	it('traps Tab and Shift+Tab inside the dialog, skipping disabled controls', async () => {
		const { container } = await openHarness();
		const input = element<HTMLInputElement>(container, '#harness-name');
		const cancel = element<HTMLButtonElement>(container, '#harness-cancel');

		cancel.focus();
		const forward = await key(cancel, 'Tab');
		expect(forward.defaultPrevented).toBe(true);
		expect(document.activeElement).toBe(input);

		const backward = await key(input, 'Tab', { shiftKey: true });
		expect(backward.defaultPrevented).toBe(true);
		expect(document.activeElement).toBe(cancel);

		// In the middle of the cycle the browser moves focus itself.
		const save = element<HTMLButtonElement>(container, '#harness-save');
		save.focus();
		const middle = await key(save, 'Tab');
		expect(middle.defaultPrevented).toBe(false);
	});

	it('ignores Escape pressed outside the dialog', async () => {
		const onclose = vi.fn();
		const { container } = await openHarness({ onclose });
		await key(document.body, 'Escape');
		expect(onclose).not.toHaveBeenCalled();
		expect(dialog(container)).not.toBeNull();
	});
});
