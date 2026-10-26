import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/auth.svelte', () => ({
	getUser: () => ({ id: 7, username: 'alice' }),
	isAdmin: () => false,
	isAuthReady: () => true,
	isLoggedIn: () => true,
}));

import AgentsPage from '../../routes/settings/agents/+page.svelte';
import BotBadge from '../components/BotBadge.svelte';
import { setTestPage } from '../test/app';
import { ApiError, bots, instance, resetTestClient, splitList } from '../test/client';
import {
	expectEscapeClosesAndRestoresFocus,
	expectModalSurvivesInteraction,
	openModalFrom,
} from '../test/modalContract';
import {
	answerConfirm,
	check,
	click,
	element,
	input,
	renderComponent,
	settle,
	submit,
	type RenderedComponent,
} from '../test/render';

const timestamp = '2026-10-03T10:00:00Z';

const bot = (id: number, username: string) => ({
	id,
	username,
	display_name: null,
	is_active: true,
	created_at: timestamp,
});

const botToken = (id: number, name: string) => ({
	id,
	name,
	scopes: 'repo',
	expires_at: null,
	last_used_at: null,
	created_at: timestamp,
	repositories: ['alice/app'],
	mcp_tools: null,
	deny_protected_merge: true,
});

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/settings/agents', {});
	bots.list.mockResolvedValue([bot(2, 'alice-agent')]);
	bots.listTokens.mockResolvedValue([botToken(5, 'claude-code')]);
	bots.getAgentProfile.mockRejectedValue(new ApiError('this bot has no agent profile', 404));
	instance.get.mockResolvedValue({ agents_enabled: true });
});

const storedProfile = (overrides: Record<string, unknown> = {}) => ({
	enabled: true,
	preset: 'claude_code',
	image: null,
	command: null,
	runner_tags: ['docker'],
	has_model_key: true,
	timeout_secs: 1800,
	max_runs_per_day: 20,
	max_iterations: 5,
	repositories: ['alice/app'],
	agents_enabled: true,
	created_at: timestamp,
	updated_at: timestamp,
	...overrides,
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

async function openTokens() {
	rendered = await renderComponent(AgentsPage);
	await click(element(rendered.container, '.manage-tokens'));
	expect(bots.listTokens).toHaveBeenCalledWith('alice-agent');
}

describe('agents settings', () => {
	it('lists the caller’s agents and creates one', async () => {
		bots.create.mockResolvedValue(bot(3, 'review-bot'));
		rendered = await renderComponent(AgentsPage);
		expect(bots.list).toHaveBeenCalledOnce();
		expect(rendered.container.textContent).toContain('@alice-agent');

		const inputs = rendered.container.querySelectorAll<HTMLInputElement>('.create-form input');
		await input(inputs[0], ' review-bot ');
		await input(inputs[1], 'Reviewer');
		await submit(element<HTMLFormElement>(rendered.container, 'form.create-form'));

		expect(bots.create).toHaveBeenCalledWith('review-bot', 'Reviewer');
		expect(bots.list).toHaveBeenCalledTimes(2);
		expect(rendered.container.textContent).toContain('Add it as a collaborator');
	});

	it('mints a narrowed token and shows the raw value once', async () => {
		bots.createToken.mockResolvedValue({ ...botToken(6, 'ci'), token: 'ifp_secret' });
		await openTokens();
		expect(rendered!.container.textContent).toContain('Repositories: alice/app');
		expect(rendered!.container.textContent).toContain('Kept off protected branches');

		const form = element<HTMLFormElement>(rendered!.container, 'form.token-form');
		const textInputs = form.querySelectorAll<HTMLInputElement>('input:not([type="checkbox"]):not([type="date"])');
		await input(textInputs[0], 'ci');
		await input(element<HTMLTextAreaElement>(form, 'textarea'), 'alice/app,\nalice/docs');
		await input(textInputs[1], 'get_issue, create_pr');
		await check(element<HTMLInputElement>(form, 'input[type="checkbox"]'), false);
		await submit(form);

		expect(bots.createToken).toHaveBeenCalledWith('alice-agent', 'ci', undefined, {
			repositories: ['alice/app', 'alice/docs'],
			mcp_tools: ['get_issue', 'create_pr'],
			deny_protected_merge: false,
		});
		expect(rendered!.container.textContent).toContain('ifp_secret');
		expect(bots.listTokens).toHaveBeenCalledTimes(2);
	});

	it('mints an unconfined token kept off protected branches by default', async () => {
		bots.createToken.mockResolvedValue({ ...botToken(6, 'plain'), token: 'ifp_plain' });
		await openTokens();
		const form = element<HTMLFormElement>(rendered!.container, 'form.token-form');
		await input(form.querySelector<HTMLInputElement>('input')!, 'plain');
		await submit(form);

		expect(bots.createToken).toHaveBeenCalledWith('alice-agent', 'plain', undefined, {
			repositories: undefined,
			mcp_tools: undefined,
			deny_protected_merge: true,
		});
	});

	it('revokes a token of the open agent', async () => {
		bots.deleteToken.mockResolvedValue(undefined);
		await openTokens();
		await click(element(rendered!.container, '.revoke-token'));
		expect(bots.deleteToken).not.toHaveBeenCalled();
		await answerConfirm();
		expect(bots.deleteToken).toHaveBeenCalledWith('alice-agent', 5);
	});

	it('deletes an agent after confirmation', async () => {
		bots.delete.mockResolvedValue(undefined);
		rendered = await renderComponent(AgentsPage);
		await click(element(rendered.container, '.delete-bot'));
		expect(bots.delete).not.toHaveBeenCalled();
		expect(await answerConfirm()).toContain('@alice-agent');
		expect(bots.delete).toHaveBeenCalledWith('alice-agent');
		expect(bots.list).toHaveBeenCalledTimes(2);
	});

	it('keeps an agent when deletion is not confirmed', async () => {
		rendered = await renderComponent(AgentsPage);
		await click(element(rendered.container, '.delete-bot'));
		await answerConfirm(false);
		expect(rendered.container.querySelector('[role="dialog"]')).toBeNull();
		expect(bots.delete).not.toHaveBeenCalled();
	});

	// card_4c186d530f59: the confirmation is the page's own dialog, so it names
	// the agent, starts on Cancel and closes on Escape without deleting.
	it('asks in a modal that starts on Cancel and closes on Escape', async () => {
		rendered = await renderComponent(AgentsPage);
		const opener = element<HTMLButtonElement>(rendered.container, '.delete-bot');
		const dialog = await openModalFrom(rendered.container, opener);
		expect(dialog.querySelector('h2')?.textContent).toBe('Delete agent');
		expect(document.activeElement).toBe(element(dialog, '.confirm-modal-cancel'));
		await expectModalSurvivesInteraction(rendered.container, element(dialog, 'h2'));
		await expectEscapeClosesAndRestoresFocus(rendered.container, opener);
		expect(bots.delete).not.toHaveBeenCalled();
	});
});

describe('agent profile', () => {
	const profileForm = () => element<HTMLFormElement>(rendered!.container, 'form.profile-form');
	const field = (label: string) => {
		const found = Array.from(profileForm().querySelectorAll('label')).find((node) =>
			node.textContent?.includes(label),
		);
		if (!found) throw new Error(`no field labelled ${label}`);
		return found.querySelector<HTMLInputElement | HTMLTextAreaElement>('input, textarea')!;
	};

	it('sets up a profile for an agent that has none, key and repositories included', async () => {
		bots.saveAgentProfile.mockResolvedValue(storedProfile());
		await openTokens();
		expect(bots.getAgentProfile).toHaveBeenCalledWith('alice-agent');
		expect(field('Model API key').getAttribute('placeholder')).toBe('No key stored');
		expect(rendered!.container.querySelector('.delete-profile')).toBeNull();

		await check(field('Start a run') as HTMLInputElement, true);
		await input(field('Model API key'), '  sk-ant-test  ');
		await input(field('Repositories it may work in'), 'alice/app\nalice/docs');
		await input(field('Runner labels'), 'docker, gpu');
		await submit(profileForm());

		expect(bots.saveAgentProfile).toHaveBeenCalledWith('alice-agent', {
			enabled: true,
			preset: 'claude_code',
			image: undefined,
			command: undefined,
			runner_tags: ['docker', 'gpu'],
			model_key: 'sk-ant-test',
			clear_model_key: undefined,
			timeout_secs: 1800,
			max_runs_per_day: 20,
			max_iterations: 5,
			repositories: ['alice/app', 'alice/docs'],
		});
		expect(rendered!.container.textContent).toContain('Agent profile saved');
		expect(field('Model API key').getAttribute('placeholder')).toContain('A key is stored');
	});

	it('keeps the stored key when the field is left empty and removes it only when asked', async () => {
		bots.getAgentProfile.mockResolvedValue(storedProfile());
		bots.saveAgentProfile.mockResolvedValue(storedProfile({ has_model_key: false }));
		await openTokens();

		await submit(profileForm());
		expect(bots.saveAgentProfile.mock.calls[0][1]).toMatchObject({
			model_key: undefined,
			clear_model_key: undefined,
			repositories: ['alice/app'],
		});

		bots.saveAgentProfile.mockResolvedValue(storedProfile({ has_model_key: false }));
		bots.getAgentProfile.mockResolvedValue(storedProfile());
		await rendered!.destroy();
		await openTokens();
		await check(field('Remove the stored key') as HTMLInputElement, true);
		await submit(profileForm());
		expect(bots.saveAgentProfile.mock.calls[1][1]).toMatchObject({
			model_key: undefined,
			clear_model_key: true,
		});
	});

	it('says when this instance does not run agents', async () => {
		instance.get.mockResolvedValue({ agents_enabled: false });
		await openTokens();
		expect(element(rendered!.container, '.agents-off').textContent).toContain('[agents] enabled = false');
	});

	it('deletes the profile after confirmation', async () => {
		bots.getAgentProfile.mockResolvedValue(storedProfile());
		bots.deleteAgentProfile.mockResolvedValue(undefined);
		await openTokens();
		await click(element(rendered!.container, '.delete-profile'));
		expect(bots.deleteAgentProfile).not.toHaveBeenCalled();
		expect(await answerConfirm()).toContain('@alice-agent');
		expect(bots.deleteAgentProfile).toHaveBeenCalledWith('alice-agent');
		expect(rendered!.container.textContent).toContain('Agent profile deleted');
	});
});

describe('bot badge', () => {
	it('marks a bot author and names its owner', async () => {
		rendered = await renderComponent(BotBadge, { owner: 'alice' });
		expect(rendered.container.textContent).toContain('bot');
		expect(rendered.container.textContent).toContain('@alice');
		expect(rendered.container.querySelector('a')?.getAttribute('href')).toBe('/alice');
	});

	it('renders no link inside a link, and nothing for a person', async () => {
		rendered = await renderComponent(BotBadge, { owner: 'alice', link: false });
		expect(rendered.container.querySelector('a')).toBeNull();
		await rendered.destroy();
		rendered = await renderComponent(BotBadge, { owner: null });
		expect(rendered.container.textContent?.trim()).toBe('');
	});
});

describe('splitList', () => {
	it('splits on commas and newlines and drops blanks', () => {
		expect(splitList(' a/b,\n c/d ,, \n')).toEqual(['a/b', 'c/d']);
		expect(splitList('')).toEqual([]);
	});
});
