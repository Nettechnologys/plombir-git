import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import CommitPage from '../../routes/[owner]/[repo]/commits/[sha]/+page.svelte';
import { setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import { button, click, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const sha = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const commit = {
	sha,
	message: 'real commit message',
	author: 'Alice',
	date: '2026-09-01T10:00:00Z',
};
const commitDiff = {
	sha,
	parent_sha: 'cccccccccccccccccccccccccccccccccccccccc',
	files_changed: [
		{
			path: 'README.md',
			status: 'modified',
			additions: 1,
			deletions: 0,
			patch: null,
			lines: [
				{ kind: 'meta', content: '@@ -1 +1,2 @@', old_line: null, new_line: null },
				{ kind: 'context', content: 'hello', old_line: 1, new_line: 1 },
				{ kind: 'addition', content: 'one more line', old_line: null, new_line: 2 },
			],
		},
		{
			path: 'textures/albedo.png',
			status: 'added',
			additions: 0,
			deletions: 0,
			patch: null,
			lines: [
				{
					kind: 'meta',
					content: 'Binary files /dev/null and b/textures/albedo.png differ',
					old_line: null,
					new_line: null,
				},
			],
		},
	],
	stats: { total_additions: 1, total_deletions: 0, files_changed: 2 },
};

beforeEach(() => {
	resetTestClient();
	repos.getCombinedStatus.mockResolvedValue({ state: 'success', total_count: 1 });
	repos.listCommitStatuses.mockResolvedValue([]);
	repos.commitSignature.mockResolvedValue(null);
	repos.log.mockResolvedValue({ commits: [commit] });
	repos.commitDiff.mockResolvedValue(commitDiff);
	setTestPage('/alice/demo/commits/' + sha, { owner: 'alice', repo: 'demo', sha });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('commit metadata availability', () => {
	it('shows an unavailable state without inventing author or time, and can retry', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
		repos.log.mockRejectedValueOnce(new Error('HTTP 503'));

		try {
			rendered = await renderComponent(CommitPage);

			expect(rendered.container.textContent).toContain('Commit details unavailable');
			expect(rendered.container.textContent).toContain('All checks passed');
			expect(rendered.container.textContent).not.toContain('Unknown');
			expect(rendered.container.textContent).not.toContain('just now');
			expect(warn).toHaveBeenCalled();

			await click(button(rendered.container, 'Retry'));

			expect(rendered.container.textContent).toContain('real commit message');
			expect(rendered.container.textContent).toContain('Alice');
			expect(rendered.container.textContent).not.toContain('Commit details unavailable');
		} finally {
			warn.mockRestore();
		}
	});

	it('does not substitute the first log entry when the requested commit is absent', async () => {
		repos.log.mockResolvedValue({
			commits: [{ ...commit, sha: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' }],
		});

		rendered = await renderComponent(CommitPage);

		expect(rendered.container.textContent).toContain('Commit not found');
		expect(rendered.container.textContent).not.toContain('real commit message');
		expect(rendered.container.textContent).not.toContain('Alice');
	});
});

// card_8c46bad6d61e: the page used to show nothing of what the commit changed.
describe('commit changed files', () => {
	it('lists every changed file with its status, counts and lines', async () => {
		rendered = await renderComponent(CommitPage);

		expect(repos.commitDiff).toHaveBeenCalledWith('alice', 'demo', sha);
		const text = rendered.container.textContent ?? '';
		expect(text).toContain('Files changed: 2');
		expect(text).toContain('README.md');
		expect(text).toContain('modified');
		expect(text).toContain('one more line');
		expect(text).toContain('textures/albedo.png');
		expect(text).toContain('added');
		// A binary file says so instead of showing git's bare meta line.
		expect(text).toContain('Binary file — no line diff to show.');
		expect(text).not.toContain('Binary files /dev/null');
	});

	it('says the files could not be loaded instead of drawing an empty commit, and can retry', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
		repos.commitDiff.mockRejectedValueOnce(new Error('HTTP 503'));

		try {
			rendered = await renderComponent(CommitPage);

			const text = rendered.container.textContent ?? '';
			expect(text).toContain('The changed files could not be loaded.');
			expect(text).not.toContain('This commit changes no files.');
			// The rest of the page is independent of the diff.
			expect(text).toContain('real commit message');
			expect(text).toContain('All checks passed');

			await click(button(rendered.container, 'Retry'));

			expect(rendered.container.textContent).toContain('README.md');
			expect(rendered.container.textContent).not.toContain('The changed files could not be loaded.');
		} finally {
			warn.mockRestore();
		}
	});

	it('states an empty commit as one', async () => {
		repos.commitDiff.mockResolvedValue({
			...commitDiff,
			files_changed: [],
			stats: { total_additions: 0, total_deletions: 0, files_changed: 0 },
		});

		rendered = await renderComponent(CommitPage);

		expect(rendered.container.textContent).toContain('This commit changes no files.');
	});
});
