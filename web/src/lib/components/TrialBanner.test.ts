import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import RegisterPage from '../../routes/register/+page.svelte';
import TrialBanner from './TrialBanner.svelte';
import { logout } from '../stores/auth.svelte';
import { setRegistrationOpen, setSetupRequired, setTrialMode } from '../stores/instance.svelte';
import { auth, resetTestClient } from '../test/client';
import { renderComponent, settle, type RenderedComponent } from '../test/render';

// card_794de0f08b9a: `plombir-git trial` prints one link, and a person who
// follows it must land on a form that already holds the setup token, on an
// instance that says on every page that it is a throwaway one.

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	await logout();
	resetTestClient();
	auth.listSsoProviders.mockResolvedValue([]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	setTrialMode(false);
	setSetupRequired(false);
	setRegistrationOpen(null);
	window.history.replaceState(null, '', '/');
	document.body.innerHTML = '';
});

describe('the trial banner', () => {
	it('says the instance is not for production while the server is a trial', async () => {
		setTrialMode(true);
		rendered = await renderComponent(TrialBanner);
		await settle();
		const banner = rendered.container.querySelector('[data-testid="trial-banner"]');
		expect(banner?.textContent).toContain('Trial mode — not for production.');
		expect(banner?.textContent).toContain('plombir-git serve');
		expect(banner?.querySelector('button')).toBeNull();
	});

	it('is absent on an instance started by serve', async () => {
		setTrialMode(false);
		rendered = await renderComponent(TrialBanner);
		await settle();
		expect(rendered.container.querySelector('[data-testid="trial-banner"]')).toBeNull();
	});
});

describe('the setup link', () => {
	it('fills in the setup token and takes it out of the address bar', async () => {
		window.history.replaceState(null, '', '/register?setup_token=tok-from-the-log&next=1');
		setRegistrationOpen(true);
		setSetupRequired(true);
		rendered = await renderComponent(RegisterPage);
		await settle();
		const field = rendered.container.querySelector<HTMLInputElement>('input[name="setup_token"]');
		expect(field?.value).toBe('tok-from-the-log');
		expect(window.location.pathname + window.location.search).toBe('/register?next=1');
	});

	it('leaves the field empty when the page was opened without one', async () => {
		window.history.replaceState(null, '', '/register');
		setRegistrationOpen(true);
		setSetupRequired(true);
		rendered = await renderComponent(RegisterPage);
		await settle();
		const field = rendered.container.querySelector<HTMLInputElement>('input[name="setup_token"]');
		expect(field?.value).toBe('');
	});
});
