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

beforeEach(() => {
	resetTestClient();
	repos.getCombinedStatus.mockResolvedValue({ state: 'success', total_count: 1 });
	repos.listCommitStatuses.mockResolvedValue([]);
	repos.commitSignature.mockResolvedValue(null);
	repos.log.mockResolvedValue({ commits: [commit] });
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
