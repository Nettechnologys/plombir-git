import { describe, expect, it } from 'vitest';

// The page's own source, pulled in by vite so the check below needs no node
// filesystem API (and no `@types/node` for `npm run check`).
import settingsPageSource from '../../routes/[owner]/[repo]/settings/branches/+page.svelte?raw';
import {
  buildBranchProtectionPayload,
  parseStringList,
  type BranchProtectionFormState
} from './branchProtectionForm';

function formState(overrides: Partial<BranchProtectionFormState> = {}): BranchProtectionFormState {
  return {
    branch_name: 'main',
    require_pr: true,
    require_status_check: true,
    required_status_checks: 'test, lint',
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
    const body = wire(buildBranchProtectionPayload(formState({ required_status_checks: '' }), false));

    expect(body).toHaveProperty('required_status_checks');
    expect(body.required_status_checks).toEqual([]);
  });

  it('drops no key on the way to the wire, whatever the form holds', () => {
    const emptied = formState({
      required_status_checks: '   ',
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

  it('still carries the lists the operator typed', () => {
    const body = wire(buildBranchProtectionPayload(formState(), true));

    expect(body).toMatchObject({
      branch_name: 'main',
      required_status_checks: ['test', 'lint'],
      allowed_push_users: ['alice', 'bob'],
      required_approvals: 2
    });
  });

  it('is the builder the settings form itself uses', () => {
    const page = settingsPageSource;

    expect(page).toContain('buildBranchProtectionPayload');
    // A second, page-local body builder would make every assertion above a
    // statement about dead code. (`parseJsonArray` is the other direction —
    // stored JSON back into the form — and is expected to stay.)
    expect(page).not.toMatch(/required_status_checks:\s*parseStringList/);
    expect(page).not.toMatch(/allowed_push_users:\s*parseStringList/);
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
