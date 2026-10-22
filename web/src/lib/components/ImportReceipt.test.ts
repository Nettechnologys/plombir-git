import { afterEach, describe, expect, it } from 'vitest';

import ImportReceipt from './ImportReceipt.svelte';
import ImportsPage from '../../routes/imports/+page.svelte';
import type { ImportReceipt as Receipt } from '$lib/api/imports';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, imports, resetTestClient } from '../test/client';
import { renderComponent, settle, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | null = null;

afterEach(async () => {
	await rendered?.destroy();
	rendered = null;
});

// The `receipt` block of `GET /api/v1/imports` for a GitLab project that kept
// its `.gitlab-ci.yml`, as `crates/rg-core/src/import/receipt.rs` serializes it.
const gitlabReceipt: Receipt = {
	objects: [
		{ kind: 'labels', source: 3, imported: 3, not_imported: 0 },
		{
			kind: 'issues',
			source: 14,
			imported: 12,
			not_imported: 2,
			missing: ['#7', '#9'],
		},
		{
			kind: 'comments',
			source: 40,
			imported: 31,
			skipped: 9,
			skipped_reason: "system notes describe the source's own events, not what a person wrote",
			not_imported: 0,
		},
	],
	refs: {
		branches: { source: 3, imported: 2, missing: ['old-release'] },
		tags: { source: 1, imported: 1 },
		default_branch: {
			name: 'main',
			source_sha: '0123456789abcdef0123456789abcdef01234567',
			imported_sha: '0123456789abcdef0123456789abcdef01234567',
			commits: 42,
		},
	},
	not_transferable: [
		"CI secret values: the source's API never returns them; add each under the repository's CI settings",
		'Webhooks and deploy keys: register them again',
	],
	ci: {
		not_run: [
			{
				path: '.gitlab-ci.yml',
				reason: 'GitLab CI is not run here: describe the pipeline as an Actions workflow',
			},
		],
	},
	templates: {
		not_read: [
			{
				path: '.gitlab/issue_templates/bug.yml',
				reason: 'Issue forms (YAML) are not offered here',
			},
		],
		not_read_omitted: 2,
	},
};

// card_a8666ed4f317: the receipt was answered by the API and shown nowhere, so
// a person who migrated through the form never learned their CI would not run.
describe('import receipt', () => {
	it('names the CI that will not run, what is missing, and the checklist', async () => {
		rendered = await renderComponent(ImportReceipt, { receipt: gitlabReceipt });
		const text = rendered.container.textContent ?? '';

		expect(text).toContain('CI that will not run here');
		expect(text).toContain('.gitlab-ci.yml');
		expect(text).toContain('GitLab CI is not run here');

		// card_04b603522857: a template in a form not read here is named too.
		expect(text).toContain('Templates that will not be offered here');
		expect(text).toContain('.gitlab/issue_templates/bug.yml');
		expect(text).toContain('Issue forms (YAML) are not offered here');
		expect(text).toContain('and 2 more');

		expect(text).toContain('Issues');
		expect(text).toContain('#7, #9');
		expect(text).toContain('9 left out on purpose');
		expect(text).toContain('old-release');
		expect(text).toContain('same commit as the source (0123456789ab)');
		expect(text).toContain('42 commits');

		const checklist = Array.from(
			rendered.container.querySelectorAll('.receipt-manual li'),
			(item) => item.textContent,
		);
		expect(checklist).toEqual(gitlabReceipt.not_transferable);

		// Something did not come over, so the receipt is open, not folded away.
		expect(rendered.container.querySelector('details')?.open).toBe(true);
		expect(text).toContain('something did not come over');
	});

	it('folds a complete receipt and shows no CI warning', async () => {
		rendered = await renderComponent(ImportReceipt, {
			receipt: {
				objects: [{ kind: 'lfs_objects', source: 2, imported: 2, not_imported: 0 }],
				refs: { branches: { source: 1, imported: 1 } },
				not_transferable: ['Webhooks and deploy keys: register them again'],
				ci: { not_run: [] },
				templates: { not_read: [] },
			},
		});
		const text = rendered.container.textContent ?? '';

		expect(rendered.container.querySelector('details')?.open).toBe(false);
		expect(text).toContain('nothing missing');
		expect(text).toContain('LFS objects');
		expect(text).not.toContain('CI that will not run here');
		expect(rendered.container.querySelector('.receipt-ci')).toBeNull();
		expect(rendered.container.querySelector('.receipt-templates')).toBeNull();
	});
});

// The page is drawn from `GET /api/v1/imports`, so the listing's receipt is the
// one a person sees: a finished task shows it, a running one has none yet.
describe('import page', () => {
	afterEach(async () => {
		await logout();
	});

	it('shows the receipt of a finished import under its task', async () => {
		resetTestClient();
		auth.me.mockResolvedValue({
			id: 7,
			username: 'alice',
			email: 'alice@example.com',
			is_admin: false,
			display_name: null,
		});
		await fetchUser();
		const task = {
			platform: 'gitlab',
			source_url: 'https://gitlab.com/acme/widgets',
			target_owner: 'alice',
			status: 'completed',
			progress: 100,
			stage: null,
			error: null,
			stats: null,
			created_at: '2026-10-22T12:00:00Z',
			updated_at: '2026-10-22T12:00:00Z',
		};
		imports.list.mockResolvedValue([
			{ ...task, id: 1, target_name: 'widgets', receipt: gitlabReceipt },
			{ ...task, id: 2, target_name: 'gadgets', status: 'running', progress: 40, receipt: null },
		]);
		setTestPage('/imports', {});
		rendered = await renderComponent(ImportsPage);
		await settle();

		const rows = Array.from(rendered.container.querySelectorAll('.table-wrap > table > tbody > tr'));
		expect(rows).toHaveLength(2);
		expect(rows[0].querySelector('.import-receipt')?.textContent).toContain('.gitlab-ci.yml');
		expect(rows[0].querySelectorAll('.receipt-manual li')).toHaveLength(2);
		expect(rows[1].querySelector('.import-receipt')).toBeNull();
	});
});
