import { describe, expect, it } from 'vitest';

// The page's own source, pulled in by vite so the check below needs no node
// filesystem API (and no `@types/node` for `npm run check`).
import settingsPageSource from '../../routes/[owner]/[repo]/settings/tags/+page.svelte?raw';
import { buildTagProtectionPayload, type TagProtectionFormState } from './tagProtectionForm';

function formState(overrides: Partial<TagProtectionFormState> = {}): TagProtectionFormState {
  return { pattern: 'v*', allowed_users: 'alice, bob', ...overrides };
}

/**
 * What the network actually carries — `JSON.stringify` is what the client does
 * with the body, and it drops any key whose value is `undefined`.
 */
function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('buildTagProtectionPayload', () => {
  it('sends the allow-list on create, so a protected pattern can have an exception', () => {
    const body = wire(buildTagProtectionPayload(formState(), true));

    expect(body).toEqual({ pattern: 'v*', allowed_users: ['alice', 'bob'] });
  });

  it('sends the allow-list on update, so an exception can be granted after the fact', () => {
    const body = wire(buildTagProtectionPayload(formState({ allowed_users: 'carol' }), false));

    expect(body).toHaveProperty('allowed_users');
    expect(body.allowed_users).toEqual(['carol']);
  });

  it('sends an emptied allow-list as [], so a granted exception can be revoked', () => {
    const body = wire(buildTagProtectionPayload(formState({ allowed_users: '  ,  ' }), false));

    expect(body).toHaveProperty('allowed_users');
    expect(body.allowed_users).toEqual([]);
  });

  it('leaves the pattern out of an update, which PATCH cannot change anyway', () => {
    const body = wire(buildTagProtectionPayload(formState(), false));

    expect(Object.keys(body)).toEqual(['allowed_users']);
  });

  it('trims the pattern the operator typed', () => {
    const body = wire(buildTagProtectionPayload(formState({ pattern: '  release/**  ' }), true));

    expect(body).toHaveProperty('pattern', 'release/**');
  });

  it('is the builder the settings form itself uses', () => {
    const page = settingsPageSource;

    expect(page).toContain('buildTagProtectionPayload');
    expect(page).toContain('tagProtections.update');
    // The defect this file guards against: the form used to hold one `pattern`
    // input and the client used to hard-code the allow-list empty, so `v*`
    // could only ever mean "nobody, not even the owner, may push this tag".
    expect(page).not.toMatch(/allowed_users:\s*\[\]/);
    expect(page).toContain('bind:value={form.allowed_users}');
  });
});
