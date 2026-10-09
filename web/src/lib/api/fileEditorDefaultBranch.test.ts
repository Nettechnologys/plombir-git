import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import EditFilePage from '../../routes/[owner]/[repo]/edit/[...path]/+page.svelte';
import NewFilePage from '../../routes/[owner]/[repo]/new/+page.svelte';
import { navigation, setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import { click, element, input, renderComponent, type RenderedComponent } from '../test/render';

// The "New file" link on the repository home carries no `?ref=`, and the
// edit page fell back to a literal `main` as well: on a repository whose
// default branch is `master` the edit form could not read the file and a new
// file was committed to a branch that did not exist (card_2e320f5287d7).

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	navigation.goto.mockReset();
	repos.get.mockResolvedValue({ id: 1, name: 'demo', default_branch: 'master' });
	repos.saveContent.mockResolvedValue({});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('file editor default branch', () => {
	it('commits a new file to the repository default branch when the URL names none', async () => {
		setTestPage('/alice/demo/new?path=notes.md', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(NewFilePage);

		expect(element<HTMLInputElement>(rendered.container, '#target-branch').value).toBe('master');
		await input(element(rendered.container, '#file-content'), 'hello');
		await click(element(rendered.container, '.form-actions .btn-primary'));
		expect(repos.saveContent).toHaveBeenCalledWith('alice', 'demo', 'notes.md', {
			branch: 'master',
			content: 'hello',
			message: 'Create notes.md',
		});
	});

	it('keeps an explicit ?ref= over the default branch', async () => {
		setTestPage('/alice/demo/new?path=notes.md&ref=dev', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(NewFilePage);

		expect(element<HTMLInputElement>(rendered.container, '#target-branch').value).toBe('dev');
		expect(repos.get).not.toHaveBeenCalled();
	});

	it('reads the file at the server HEAD and targets the default branch without ?ref=', async () => {
		repos.blob.mockResolvedValue({ content: 'old', size: 3, sha: 'aaaa' });
		setTestPage('/alice/demo/edit/notes.md', { owner: 'alice', repo: 'demo', path: 'notes.md' });
		rendered = await renderComponent(EditFilePage);

		expect(repos.blob).toHaveBeenCalledWith('alice', 'demo', 'notes.md', undefined);
		expect(element<HTMLInputElement>(rendered.container, '#target-branch').value).toBe('master');
	});

	it('leaves the branch for the author to name when the default cannot be read', async () => {
		repos.get.mockRejectedValue(new Error('HTTP 503'));
		setTestPage('/alice/demo/new?path=notes.md', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(NewFilePage);

		expect(element<HTMLInputElement>(rendered.container, '#target-branch').value).toBe('');
		await input(element(rendered.container, '#file-content'), 'hello');
		await click(element(rendered.container, '.form-actions .btn-primary'));
		expect(repos.saveContent).not.toHaveBeenCalled();
	});
});
