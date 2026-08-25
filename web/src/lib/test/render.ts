import { mount, tick, unmount, type Component } from 'svelte';

export type RenderedComponent = {
	container: HTMLDivElement;
	destroy: () => Promise<void>;
};

export async function settle(): Promise<void> {
	for (let pass = 0; pass < 10; pass += 1) {
		await Promise.resolve();
		await tick();
	}
}

export async function renderComponent(
	component: Component<any>,
	props: Record<string, unknown> = {},
): Promise<RenderedComponent> {
	const container = document.createElement('div');
	document.body.append(container);
	const instance = mount(component, { target: container, props });
	await settle();

	return {
		container,
		destroy: async () => {
			await unmount(instance);
			container.remove();
		},
	};
}

export async function input(
	element: HTMLInputElement | HTMLTextAreaElement,
	value: string,
): Promise<void> {
	element.value = value;
	element.dispatchEvent(new Event('input', { bubbles: true }));
	await settle();
}

export async function click(element: Element): Promise<void> {
	element.dispatchEvent(new MouseEvent('click', { bubbles: true }));
	await settle();
}

export function element<T extends Element>(container: ParentNode, selector: string): T {
	const found = container.querySelector<T>(selector);
	if (!found) throw new Error(`Rendered DOM is missing ${selector}`);
	return found;
}

export function button(container: ParentNode, label: string): HTMLButtonElement {
	const found = Array.from(container.querySelectorAll('button')).find(
		(candidate) => candidate.textContent?.trim() === label,
	);
	if (!found) throw new Error(`Rendered DOM is missing button "${label}"`);
	return found;
}

export async function change(
	element: HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement,
	value: string,
): Promise<void> {
	element.value = value;
	element.dispatchEvent(new Event('change', { bubbles: true }));
	await settle();
}

export async function check(element: HTMLInputElement, checked: boolean): Promise<void> {
	element.checked = checked;
	element.dispatchEvent(new Event('change', { bubbles: true }));
	await settle();
}

export async function submit(form: HTMLFormElement): Promise<void> {
	form.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true }));
	await settle();
}
