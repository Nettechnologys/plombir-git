import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import RepoVariablesPage from '../../routes/[owner]/[repo]/settings/ci-variables/+page.svelte';
import OrgVariablesPage from '../../routes/orgs/[name]/variables/+page.svelte';
import { setTestPage } from '../test/app';
import { ciVariables, resetTestClient } from '../test/client';
import {
	answerConfirm,
	button,
	click,
	element,
	input,
	renderComponent,
	settle,
	submit,
	type RenderedComponent,
} from '../test/render';

type Deferred<T> = { promise: Promise<T>; resolve: (value: T) => void };

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((resolvePromise) => {
		resolve = resolvePromise;
	});
	return { promise, resolve };
}

const timestamp = '2026-10-21T00:00:00Z';
const variable = (name: string, value: string) => ({
	name,
	value,
	created_at: timestamp,
	updated_at: timestamp,
});

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

// card_a6f59b3d45f5: the `vars.*` settings — a repository's and an
// organization's — on the one component both pages render.
describe('CI variables pages', () => {
	it('saves a repository variable once, upper-cased, and keeps the list a late first load would replace', async () => {
		setTestPage('/alice/demo/settings/ci-variables', { owner: 'alice', repo: 'demo' });
		const initial = deferred<ReturnType<typeof variable>[]>();
		const mutation = deferred<void>();
		ciVariables.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([variable('REGION', 'eu-west')]);
		ciVariables.put.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(RepoVariablesPage);

		await input(element(rendered.container, '#variable-name'), 'region');
		await input(element(rendered.container, '#variable-value'), 'eu-west');
		const form = element<HTMLFormElement>(rendered.container, 'form');
		await submit(form);
		expect(button(rendered.container, 'Save variable').disabled).toBe(true);
		await submit(form);
		expect(ciVariables.put).toHaveBeenCalledOnce();
		expect(ciVariables.put).toHaveBeenCalledWith(
			{ kind: 'repo', owner: 'alice', repo: 'demo' },
			'REGION',
			'eu-west',
		);

		mutation.resolve(undefined);
		await settle();
		expect(rendered.container.textContent).toContain('eu-west');

		initial.resolve([variable('STALE', 'old')]);
		await settle();
		expect(rendered.container.textContent).toContain('REGION');
		expect(rendered.container.textContent).not.toContain('STALE');
	});

	it('lists and deletes an organization variable through the organization scope', async () => {
		setTestPage('/orgs/acme/variables', { name: 'acme' });
		ciVariables.list
			.mockResolvedValueOnce([variable('TIER', 'gold')])
			.mockResolvedValueOnce([]);
		ciVariables.delete.mockResolvedValueOnce(undefined);
		rendered = await renderComponent(OrgVariablesPage);
		await settle();
		expect(ciVariables.list).toHaveBeenCalledWith({ kind: 'org', name: 'acme' });
		expect(rendered.container.textContent).toContain('gold');

		await click(button(rendered.container, 'Delete'));
		await answerConfirm(true);
		await settle();
		expect(ciVariables.delete).toHaveBeenCalledWith({ kind: 'org', name: 'acme' }, 'TIER');
		expect(rendered.container.textContent).toContain('No variables configured.');
	});
});
