import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import DashboardPage from '../../routes/dashboard/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, orgs, repos, resetTestClient } from '../test/client';
import {
	click,
	element,
	input,
	renderComponent,
	submit,
	type RenderedComponent,
} from '../test/render';

const timestamp = '2026-09-06T12:00:00Z';

function user() {
	return {
		id: 1,
		username: 'alice',
		email: 'alice@example.test',
		is_admin: false,
		display_name: 'Alice',
	};
}

function organization(name: string) {
	return {
		id: 1,
		name,
		display_name: name,
		description: `${name} description`,
		visibility: 'public',
		created_at: timestamp,
	};
}

function emptyRepositories() {
	return {
		data: [],
		pagination: {
			page: 1,
			per_page: 20,
			total: 0,
			total_pages: 1,
			has_next: false,
			has_prev: false,
		},
	};
}

function template(name: string) {
	return { data: [{ key: name, name, description: `${name} description` }] };
}

function prepareTemplates(): void {
	repos.templates.gitignores.mockResolvedValue(template('Node'));
	repos.templates.licenses.mockResolvedValue(template('MIT'));
	repos.templates.readmes.mockResolvedValue(template('Default README'));
	repos.templates.labels.mockResolvedValue(template('Default labels'));
}

let rendered: RenderedComponent | undefined;
let warn: ReturnType<typeof vi.spyOn>;

beforeEach(async () => {
	await logout();
	resetTestClient();
	warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
	setTestPage('/dashboard', {});
	auth.me.mockResolvedValueOnce(user());
	repos.list.mockResolvedValue(emptyRepositories());
	prepareTemplates();
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	warn.mockRestore();
	await logout();
});

async function openCreateForm(): Promise<void> {
	rendered = await renderComponent(DashboardPage);
	await click(element(rendered.container, '.dashboard-header .btn-primary'));
}

describe('dashboard optional section availability', () => {
	it('does not treat a failed organization read as a personal-only account', async () => {
		orgs.list.mockRejectedValue(new Error('HTTP 500'));
		setTestPage('/dashboard?owner=acme', {});

		rendered = await renderComponent(DashboardPage);

		const notice = element(rendered!.container, '.owner-availability');
		expect(notice.textContent).toContain('Repository owner options could not be loaded.');
		expect(notice.textContent).toContain('The shown target is acme.');
		expect(rendered!.container.querySelector('.owner-select')).toBeNull();
		expect(element<HTMLButtonElement>(rendered!.container, 'button[type="submit"]').disabled).toBe(true);

		await input(element(rendered!.container, 'form input[type="text"]'), 'must-not-land-personally');
		await submit(element<HTMLFormElement>(rendered!.container, 'form'));
		expect(repos.create).not.toHaveBeenCalled();
		expect(warn).toHaveBeenCalledWith(
			'Could not load organization ownership options:',
			expect.any(Error),
		);
	});

	it('retries organization ownership and enables creation only after a real answer', async () => {
		orgs.list
			.mockRejectedValueOnce(new Error('HTTP 503'))
			.mockResolvedValueOnce([organization('acme')]);

		await openCreateForm();
		await click(element(rendered!.container, '.owner-availability button'));

		expect(orgs.list).toHaveBeenCalledTimes(2);
		expect(rendered!.container.textContent).toContain('acme');
		expect(rendered!.container.querySelector('.owner-availability')).toBeNull();
		expect(element<HTMLButtonElement>(rendered!.container, 'button[type="submit"]').disabled).toBe(false);
	});

	it('keeps a successful empty organization list quiet and personal', async () => {
		orgs.list.mockResolvedValue([]);

		await openCreateForm();

		expect(rendered!.container.querySelector('.owner-availability')).toBeNull();
		expect(rendered!.container.querySelector('.owner-select')).toBeNull();
		expect(element<HTMLButtonElement>(rendered!.container, 'button[type="submit"]').disabled).toBe(false);
		expect(warn).not.toHaveBeenCalled();
	});

	it('reports and retries an unavailable template catalog', async () => {
		orgs.list.mockResolvedValue([]);
		repos.templates.gitignores
			.mockRejectedValueOnce(new Error('HTTP 502'))
			.mockResolvedValueOnce(template('Recovered ignore'));

		await openCreateForm();
		const notice = element(rendered!.container, '.template-availability');
		expect(notice.textContent).toContain('Repository templates could not be loaded.');
		expect(warn).toHaveBeenCalledWith('Could not load repository templates:', expect.any(Error));

		await click(element(notice, 'button'));

		expect(repos.templates.gitignores).toHaveBeenCalledTimes(2);
		expect(rendered!.container.textContent).toContain('Recovered ignore');
		expect(rendered!.container.querySelector('.template-availability')).toBeNull();
	});
});
