import { createRawSnippet } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import RootLayout from '../../routes/+layout.svelte';
import LandingPage from '../../routes/+page.svelte';
import SourceFooter from '../components/SourceFooter.svelte';
import { logout } from '../stores/auth.svelte';
import { getSourceLink, setSourceLink } from '../stores/instance.svelte';
import { instance, repos, resetTestClient } from '../test/client';
import { renderComponent, type RenderedComponent } from '../test/render';

// card_0960f17d6aeb: every page offers the source of the running build (AGPL
// §13), at the address the operator configured. The fixture is a fork on
// another host on purpose — a page that ignored the server and linked upstream
// is exactly the defect, and it would pass against upstream's own URL.
const COMMIT = '87bd02a3c1f0e9d8b7a6c5d4e3f2a1b0c9d8e7f6';
const FORK = 'https://codeberg.org/someone/forge-fork';
const FORK_AT_COMMIT = `${FORK}/tree/${COMMIT}`;
const UPSTREAM = 'github.com/Nettechnologys';

let rendered: RenderedComponent | undefined;

function instanceInfo(source_url: string, source_commit: string | null) {
	return {
		maintenance_mode: false,
		banner_message: null,
		banner_type: 'info',
		attestation_enabled: false,
		source_url,
		source_commit,
	};
}

beforeEach(async () => {
	resetTestClient();
	await logout();
	resetTestClient();
	setSourceLink(null);
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => new Response(JSON.stringify({ status: 'ok' }), { status: 200 })),
	);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

function sourceAnchors(container: ParentNode): HTMLAnchorElement[] {
	return [...container.querySelectorAll<HTMLAnchorElement>('footer a')].filter((a) =>
		a.href.startsWith(FORK),
	);
}

describe('source code offer', () => {
	it('links the commit the server reported, from every page through the root layout', async () => {
		instance.get.mockResolvedValue(instanceInfo(FORK_AT_COMMIT, COMMIT));

		rendered = await renderComponent(RootLayout, {
			children: createRawSnippet(() => ({ render: () => '<p>page</p>' })),
		});

		expect(instance.get).toHaveBeenCalledTimes(1);
		const [link] = sourceAnchors(rendered.container);
		expect(link?.getAttribute('href')).toBe(FORK_AT_COMMIT);
		expect(link?.getAttribute('rel')).toContain('noopener');
		expect(rendered.container.querySelector('.source-footer code')?.textContent).toBe(
			COMMIT.slice(0, 12),
		);
	});

	it('offers no link at all when the server never said where its source is', async () => {
		instance.get.mockRejectedValue(new Error('instance endpoint unavailable'));

		rendered = await renderComponent(RootLayout, {
			children: createRawSnippet(() => ({ render: () => '<p>page</p>' })),
		});

		expect(getSourceLink()).toBeNull();
		expect(rendered.container.querySelector('.source-footer')).toBeNull();
		// Above all, no fallback to upstream: for a modified fork that is the
		// one wrong answer.
		expect(rendered.container.innerHTML).not.toContain(UPSTREAM);
	});

	it('names an unrecorded commit as unknown instead of showing one', async () => {
		setSourceLink({ url: FORK, commit: null });

		rendered = await renderComponent(SourceFooter);

		const link = rendered.container.querySelector<HTMLAnchorElement>('.source-footer a');
		expect(link?.getAttribute('href')).toBe(FORK);
		expect(link?.getAttribute('title')).toContain('not recorded');
		expect(rendered.container.querySelector('.source-footer code')).toBeNull();
	});

	it("sends a logged-out visitor of the landing page to the instance's source, not upstream's", async () => {
		repos.explore.mockResolvedValue({ data: [] });
		setSourceLink({ url: FORK_AT_COMMIT, commit: COMMIT });

		rendered = await renderComponent(LandingPage);

		const [link] = sourceAnchors(rendered.container);
		expect(link?.getAttribute('href')).toBe(FORK_AT_COMMIT);
		expect(rendered.container.innerHTML).not.toContain(UPSTREAM);
	});
});
