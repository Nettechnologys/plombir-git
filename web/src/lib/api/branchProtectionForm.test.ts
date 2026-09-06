import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import SettingsPage from '../../routes/[owner]/[repo]/settings/branches/+page.svelte';
import {
  buildBranchProtectionPayload,
  parseStoredStringList,
  parseStringList,
  type BranchProtectionFormState
} from './branchProtectionForm';
import { setTestPage } from '../test/app';
import { branchProtections, resetTestClient } from '../test/client';
import { button, click, element, input, renderComponent, submit, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/settings/branches', { owner: 'alice', repo: 'demo' });
	branchProtections.list.mockResolvedValue([
		{
			id: 7,
			branch_name: 'main',
			require_pr: true,
			require_status_check: true,
			required_status_checks: '["test","lint"]',
			require_approval: true,
			required_approvals: 2,
			allow_force_push: false,
			require_signed_commits: false,
			allowed_push_users: [{ username: 'alice' }],
		},
	]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

function formState(overrides: Partial<BranchProtectionFormState> = {}): BranchProtectionFormState {
  return {
    branch_name: 'main',
    require_pr: true,
    require_status_check: true,
    required_status_checks: ['test', 'lint'],
    require_approval: true,
    required_approvals: 2,
    allow_force_push: false,
    require_signed_commits: false,
    allowed_push_users: 'alice, bob',
    ...overrides
  };
}

/**
 * What the network actually carries. The bug this file guards against is
 * invisible on the object itself — `{a: undefined}` still has the key — and
 * only shows up once `JSON.stringify` has dropped it, which is what
 * `branchProtections.create` / `.update` do with the body.
 */
function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('buildBranchProtectionPayload', () => {
  it('sends an emptied allow-list as [], so a direct-push grant can be revoked', () => {
    const body = wire(buildBranchProtectionPayload(formState({ allowed_push_users: '' }), false));

    expect(body).toHaveProperty('allowed_push_users');
    expect(body.allowed_push_users).toEqual([]);
  });

  it('names the people on the allow-list, because an id is not something an owner can look up', () => {
    const body = wire(buildBranchProtectionPayload(formState(), false));

    expect(body.allowed_push_users).toEqual(['alice', 'bob']);
    // The numeric field the form used to fill is not sent at all: the API reads
    // the named list as authoritative and would otherwise have two to choose
    // between.
    expect(body).not.toHaveProperty('allowed_push_user_ids');
  });

  it('sends emptied status checks as [], so the rule can go back to "any green CI"', () => {
    const body = wire(buildBranchProtectionPayload(formState({ required_status_checks: [] }), false));

    expect(body).toHaveProperty('required_status_checks');
    expect(body.required_status_checks).toEqual([]);
  });

  it('drops no key on the way to the wire, whatever the form holds', () => {
    const emptied = formState({
      required_status_checks: [''],
      allowed_push_users: ' , ',
      require_status_check: false,
      require_approval: false,
      required_approvals: ''
    });

    const update = wire(buildBranchProtectionPayload(emptied, false));
    expect(Object.keys(update).sort()).toEqual([
      'allow_force_push',
      'allowed_push_users',
      'require_approval',
      'require_pr',
      'require_signed_commits',
      'require_status_check',
      'required_approvals',
      'required_status_checks'
    ]);

    const create = wire(buildBranchProtectionPayload(emptied, true));
    expect(create).toHaveProperty('branch_name', 'main');
  });

  it('keeps each status-check row intact, including commas and whitespace', () => {
    const body = wire(buildBranchProtectionPayload(formState({
      required_status_checks: ['lint, security', ' deploy ']
    }), true));

    expect(body).toMatchObject({
      branch_name: 'main',
      required_status_checks: ['lint, security', ' deploy '],
      allowed_push_users: ['alice', 'bob'],
      required_approvals: 2
    });
  });

	it('round-trips one stored check containing a comma as one check', async () => {
		branchProtections.list.mockResolvedValue([
			{
				id: 7,
				branch_name: 'main',
				require_pr: true,
				require_status_check: true,
				required_status_checks: '["lint, security"]',
				require_approval: true,
				required_approvals: 2,
				allow_force_push: false,
				require_signed_commits: false,
				allowed_push_users: [{ username: 'alice' }],
			},
		]);

		rendered = await renderComponent(SettingsPage);
		await click(button(rendered.container, 'Edit'));
		expect(element<HTMLInputElement>(rendered.container, '#required-check-0').value).toBe('lint, security');
		await submit(element(rendered.container, '.rule-form'));

		expect(branchProtections.update).toHaveBeenCalledWith(
			'alice',
			'demo',
			7,
			expect.objectContaining({ required_status_checks: ['lint, security'] }),
		);
	});

	it('adds a multidimensional matrix job name as one required check', async () => {
		branchProtections.list.mockResolvedValue([
			{
				id: 7,
				branch_name: 'main',
				require_pr: true,
				require_status_check: true,
				required_status_checks: '[]',
				require_approval: true,
				required_approvals: 2,
				allow_force_push: false,
				require_signed_commits: false,
				allowed_push_users: [{ username: 'alice' }],
			},
		]);

		rendered = await renderComponent(SettingsPage);
		await click(button(rendered.container, 'Edit'));
		await click(button(rendered.container, 'Add required check'));
		await input(element(rendered.container, '#required-check-0'), 'test [os=linux, rust=stable]');
		await input(element(rendered.container, '#allowed-pushers'), ' bob, carol ');
		await submit(element(rendered.container, '.rule-form'));

		expect(branchProtections.update).toHaveBeenCalledWith(
			'alice',
			'demo',
			7,
			expect.objectContaining({
				required_status_checks: ['test [os=linux, rust=stable]'],
				allowed_push_users: ['bob', 'carol'],
			}),
		);
	});

	it('removes exactly one required check without reparsing its siblings', async () => {
		branchProtections.list.mockResolvedValue([
			{
				id: 7,
				branch_name: 'main',
				require_pr: true,
				require_status_check: true,
				required_status_checks: '["lint, security","test"]',
				require_approval: true,
				required_approvals: 2,
				allow_force_push: false,
				require_signed_commits: false,
				allowed_push_users: [{ username: 'alice' }],
			},
		]);

		rendered = await renderComponent(SettingsPage);
		await click(button(rendered.container, 'Edit'));
		await click(element(rendered.container, '.remove-required-check'));
		await submit(element(rendered.container, '.rule-form'));

		expect(branchProtections.update).toHaveBeenCalledWith(
			'alice',
			'demo',
			7,
			expect.objectContaining({ required_status_checks: ['test'] }),
		);
	});
});

describe('stored status-check list parsing', () => {
  it('keeps null and [] as honest empty lists', () => {
    expect(parseStoredStringList(null)).toEqual({ kind: 'parsed', value: [] });
    expect(parseStoredStringList('[]')).toEqual({ kind: 'parsed', value: [] });
  });

  it('keeps invalid JSON and wrong JSON shapes unavailable', () => {
    expect(parseStoredStringList('not-json')).toEqual({ kind: 'unavailable' });
    expect(parseStoredStringList('')).toEqual({ kind: 'unavailable' });
    expect(parseStoredStringList('{"test":true}')).toEqual({ kind: 'unavailable' });
    expect(parseStoredStringList('["test",7]')).toEqual({ kind: 'unavailable' });
  });

  it('blocks a form round trip until the operator explicitly replaces an unreadable value', async () => {
    branchProtections.list.mockResolvedValue([
      {
        id: 7,
        branch_name: 'main',
        require_pr: true,
        require_status_check: true,
        required_status_checks: 'not-json',
        require_approval: true,
        required_approvals: 2,
        allow_force_push: false,
        require_signed_commits: false,
        allowed_push_users: [{ username: 'alice' }],
      },
    ]);

    rendered = await renderComponent(SettingsPage);
    await click(button(rendered.container, 'Edit'));

    expect(rendered.container.querySelector('#required-checks')).toBeNull();
    expect(element(rendered.container, '.stored-value-error').textContent).toContain(
      'could not be read',
    );

    await submit(element(rendered.container, '.rule-form'));
    expect(branchProtections.update).not.toHaveBeenCalled();
    expect(element(rendered.container, '.error-box').textContent).toContain('cannot be saved');

    await click(element(rendered.container, '.replace-unreadable-checks'));
    expect(rendered.container.querySelectorAll('.status-check-row')).toHaveLength(0);
    await submit(element(rendered.container, '.rule-form'));

    expect(branchProtections.update).toHaveBeenCalledWith(
      'alice',
      'demo',
      7,
      expect.objectContaining({ required_status_checks: [] }),
    );
  });

  it('renders a stored [] as the normal empty editor without a warning', async () => {
    branchProtections.list.mockResolvedValue([
      {
        id: 7,
        branch_name: 'main',
        require_pr: true,
        require_status_check: true,
        required_status_checks: '[]',
        require_approval: true,
        required_approvals: 2,
        allow_force_push: false,
        require_signed_commits: false,
        allowed_push_users: [{ username: 'alice' }],
      },
    ]);

    rendered = await renderComponent(SettingsPage);
    await click(button(rendered.container, 'Edit'));

    expect(rendered.container.querySelectorAll('.status-check-row')).toHaveLength(0);
    expect(button(rendered.container, 'Add required check')).toBeTruthy();
    expect(rendered.container.querySelector('.stored-value-error')).toBeNull();
  });
});

describe('list parsing', () => {
  it('reads an empty or blank field as an empty list, never as "leave it alone"', () => {
    expect(parseStringList('')).toEqual([]);
    expect(parseStringList('  ,  ')).toEqual([]);
  });

  it('keeps every entry the operator typed, trimmed', () => {
    // Nothing is dropped for not looking like a name: a bare id still resolves
    // (`UserRef::from_identifier` reads it as one), and an entry that matches
    // nobody is a `400` naming it rather than a silent omission.
    expect(parseStringList(' alice , 42,, bob ')).toEqual(['alice', '42', 'bob']);
  });
});
