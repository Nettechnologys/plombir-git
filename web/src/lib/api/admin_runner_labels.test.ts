import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/auth.svelte', () => ({
	getUser: () => ({ id: 999, username: 'admin' }),
	isAdmin: () => true,
	isAuthReady: () => true,
	isLoggedIn: () => true,
}));

import AdminRunnersPage from '../../routes/admin/runners/+page.svelte';
import { setTestPage } from '../test/app';
import { resetTestClient, runners } from '../test/client';
import { click, element, input, renderComponent, settle, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const pagination = {
	page: 1,
	per_page: 20,
	total: 0,
	total_pages: 1,
};

beforeEach(() => {
	resetTestClient();
	setTestPage('/admin/runners', {});
	runners.list.mockResolvedValue({ data: [], pagination });
	runners.register.mockResolvedValue({ id: 7, token: 'runner-secret' });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

async function renderRegistrationForm() {
	rendered = await renderComponent(AdminRunnersPage);
	await input(element(rendered.container, '.form-grid > label:first-child input'), 'builder-1');
	await input(element(rendered.container, '.form-grid > label:nth-child(2) input'), 'owner/project');
}

async function addLabel(value: string) {
	await click(element(rendered!.container, '.add-runner-label'));
	const inputs = rendered!.container.querySelectorAll<HTMLInputElement>('.runner-label-row input');
	await input(inputs[inputs.length - 1], value);
}

describe('admin runner label registration', () => {
	it('submits a comma-bearing label as one element next to a separate label', async () => {
		await renderRegistrationForm();
		await addLabel('gpu,a100');
		await addLabel('linux');
		await click(element(rendered!.container, '.form-grid > .btn-primary'));
		await settle();

		expect(runners.register).toHaveBeenCalledWith({
			repository: 'owner/project',
			name: 'builder-1',
			labels: ['gpu,a100', 'linux'],
		});
	});

	it('omits labels when the structural editor is empty', async () => {
		await renderRegistrationForm();
		await addLabel('');
		await click(element(rendered!.container, '.form-grid > .btn-primary'));
		await settle();

		expect(runners.register).toHaveBeenCalledWith({
			repository: 'owner/project',
			name: 'builder-1',
			labels: undefined,
		});
	});
});
