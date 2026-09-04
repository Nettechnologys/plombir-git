import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import CommitPage from '../../routes/[owner]/[repo]/commits/[sha]/+page.svelte';
import { setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import { renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const sha = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';

beforeEach(() => {
	resetTestClient();
	repos.getCombinedStatus.mockResolvedValue({ state: 'success', total_count: 0 });
	repos.listCommitStatuses.mockResolvedValue([]);
	repos.log.mockResolvedValue({
		commits: [{ sha, message: 'a signed commit', author: 'alice', date: '2026-09-01T10:00:00Z' }],
	});
	setTestPage('/alice/demo/commits/' + sha, { owner: 'alice', repo: 'demo', sha });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('commit signature badge', () => {
	// The defect this file exists for (card_61b29791d099). `%G?` = `E` means Git
	// could not check the signature — this instance has no public key for the
	// signer. On an instance where contributor keys were never imported that is
	// the answer for *every* signed commit, and the badge used to draw all of
	// them as "✗ Bad signature": an accusation of forgery against commits nobody
	// has a complaint about.
	it('renders a signature it could not check separately from a bad one', async () => {
		repos.commitSignature.mockResolvedValue({
			verdict: 'undeterminable',
			signer_key: 'ABCD1234',
			signer_name: 'Alice',
			signer_email: 'alice@example.com',
			status: 'unverifiable',
		});

		rendered = await renderComponent(CommitPage);

		expect(rendered.container.textContent).toContain('Signature could not be checked');
		// The three states this must not collapse into: an accusation, a claim
		// the commit is unsigned, and a claim the signature verified.
		expect(rendered.container.textContent).not.toContain('Bad signature');
		expect(rendered.container.textContent).not.toContain('Unsigned');
		expect(rendered.container.textContent).not.toContain('Signed');
	});

	// The regression half: a verdict Git actually reached must keep its full
	// weight. Neutralizing this one would be the mirror-image defect.
	it('still renders a rejected signature as a bad one', async () => {
		repos.commitSignature.mockResolvedValue({
			verdict: 'invalid',
			signer_key: 'ABCD1234',
			signer_name: 'Alice',
			signer_email: 'alice@example.com',
			status: 'bad_signature',
		});

		rendered = await renderComponent(CommitPage);

		expect(rendered.container.textContent).toContain('Bad signature');
		expect(rendered.container.textContent).not.toContain('Signature could not be checked');
	});

	it('renders a valid signature as signed, with the signer', async () => {
		repos.commitSignature.mockResolvedValue({
			verdict: 'valid',
			signer_key: 'ABCD1234',
			signer_name: 'Alice',
			signer_email: 'alice@example.com',
			status: 'valid',
		});

		rendered = await renderComponent(CommitPage);

		expect(rendered.container.textContent).toContain('Signed');
		expect(rendered.container.textContent).toContain('by Alice');
		expect(rendered.container.textContent).not.toContain('Bad signature');
	});

	it('renders a commit with no signature as unsigned, not as a failed check', async () => {
		repos.commitSignature.mockResolvedValue({
			verdict: 'unsigned',
			signer_key: null,
			signer_name: null,
			signer_email: null,
			status: 'no_signature',
		});

		rendered = await renderComponent(CommitPage);

		expect(rendered.container.textContent).toContain('Unsigned');
		expect(rendered.container.textContent).not.toContain('Bad signature');
		expect(rendered.container.textContent).not.toContain('Signature could not be checked');
	});

	// The transport half (card_c84bb28a36e1). A signature endpoint that never
	// answered used to be swallowed into `null`, and a `null` signature draws no
	// badge at all — so on an instance whose git gateway was down every signed
	// commit looked exactly like a commit the server had told us nothing about,
	// which is what "unsigned" looks like to a reader too.
	it('separates a signature it could not ask about from one the server answered for', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
		repos.commitSignature.mockRejectedValue(new Error('HTTP 500'));

		try {
			rendered = await renderComponent(CommitPage);

			expect(rendered.container.textContent).toContain('Signature status unavailable');
			expect(rendered.container.textContent).not.toContain('Unsigned');
			expect(rendered.container.textContent).not.toContain('Signature could not be checked');
			expect(rendered.container.textContent).not.toContain('Bad signature');
			expect(warn).toHaveBeenCalled();
		} finally {
			warn.mockRestore();
		}
	});

	// The regression half: an answer that did arrive keeps its own words.
	it('does not claim the check was unreachable when the server said unsigned', async () => {
		repos.commitSignature.mockResolvedValue({
			verdict: 'unsigned',
			signer_key: null,
			signer_name: null,
			signer_email: null,
			status: 'no_signature',
		});

		rendered = await renderComponent(CommitPage);

		expect(rendered.container.textContent).toContain('Unsigned');
		expect(rendered.container.textContent).not.toContain('Signature status unavailable');
	});

	// The badge's tooltip carries the detail behind the verdict. `E` is "cannot
	// be checked", never an expiry — the old table labelled it `expired`, which
	// told the reader a wrong fact even where the verdict was neutral.
	it('never labels an unchecked signature as expired', async () => {
		repos.commitSignature.mockResolvedValue({
			verdict: 'undeterminable',
			signer_key: null,
			signer_name: null,
			signer_email: null,
			status: 'unverifiable',
		});

		rendered = await renderComponent(CommitPage);

		const badge = rendered.container.querySelector('.gpg-badge');
		expect(badge?.getAttribute('title')).toBe('GPG: unverifiable');
		expect(badge?.getAttribute('title')).not.toContain('expired');
	});
});
