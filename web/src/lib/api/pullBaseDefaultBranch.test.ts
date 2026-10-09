import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import PullListPage from '../../routes/[owner]/[repo]/pulls/+page.svelte';
import { setTestPage } from '../test/app';
import { pulls, repos, resetTestClient } from '../test/client';
import { button, change, click, element, input, renderComponent, submit, type RenderedComponent } from '../test/render';

// The new-pull-request form started with a literal `main` bound to its base
// select. On a repository without a `main` branch the select showed nothing
// chosen and the request went out with `base_branch: "main"`
// (card_2e320f5287d7, sideways).

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	pulls.list.mockResolvedValue({ data: [], pagination: { page: 1, per_page: 20, total: 0, total_pages: 1 } });
	pulls.template.mockResolvedValue(null);
	pulls.create.mockResolvedValue({ id: 1, number: 1, title: 'x' });
	repos.get.mockResolvedValue({ owner_id: 1, default_branch: 'master' });
	repos.branches.mockResolvedValue([
		{ name: 'dev', is_default: false },
		{ name: 'master', is_default: true },
	]);
	repos.forks.mockResolvedValue({ data: [], pagination: { page: 1, per_page: 100, total: 0, total_pages: 1 } });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('pull request base branch', () => {
	it('defaults the base to the repository default branch, not a literal main', async () => {
		setTestPage('/alice/demo/pulls', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PullListPage);
		await click(button(rendered.container, 'New Pull Request'));

		const selects = Array.from(rendered.container.querySelectorAll<HTMLSelectElement>('.branch-row select'));
		const baseSelect = selects[selects.length - 1];
		expect(baseSelect.value).toBe('master');

		await change(selects[0], 'dev');
		await input(element(rendered.container, '.create-form input[type="text"]'), 'Feature');
		await submit(element<HTMLFormElement>(rendered.container, '.create-form form'));

		expect(pulls.create).toHaveBeenCalledWith('alice', 'demo', expect.objectContaining({
			head_branch: 'dev',
			base_branch: 'master',
		}));
	});
});
