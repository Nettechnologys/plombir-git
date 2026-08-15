import { describe, expect, it } from 'vitest';

// The page's own source, pulled in by vite so the check below needs no node
// filesystem API (and no `@types/node` for `npm run check`).
import editPageSource from '../../routes/[owner]/[repo]/releases/edit/[id]/+page.svelte?raw';
import { buildReleaseUpdatePayload, type ReleaseUpdateFormState } from './releaseForm';

function formState(overrides: Partial<ReleaseUpdateFormState> = {}): ReleaseUpdateFormState {
  return {
    title: 'v2.0.0',
    body: 'Fixed the thing.',
    is_draft: false,
    is_prerelease: false,
    ...overrides
  };
}

/** What the network actually carries — `JSON.stringify` drops an `undefined` value. */
function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('buildReleaseUpdatePayload', () => {
  it('sends emptied release notes, so published text can be taken back down', () => {
    const body = wire(buildReleaseUpdatePayload(formState({ body: '   ' })));

    // A dropped key reads as "keep the notes", which is why deleting the text
    // and saving used to answer 200 and change nothing.
    expect(body).toHaveProperty('body', '');
  });

  it('drops no key on the way to the wire, whatever the form holds', () => {
    const body = wire(buildReleaseUpdatePayload(formState({ title: '', body: '' })));

    expect(Object.keys(body).sort()).toEqual(['body', 'is_draft', 'is_prerelease', 'title']);
  });

  it('still carries what the author typed, and the two flags', () => {
    const body = wire(
      buildReleaseUpdatePayload(formState({ title: '  v2.0.1  ', is_prerelease: true }))
    );

    expect(body).toEqual({
      title: 'v2.0.1',
      body: 'Fixed the thing.',
      is_draft: false,
      is_prerelease: true
    });
  });

  it('is the builder the edit form itself uses', () => {
    const page = editPageSource;

    expect(page).toContain('buildReleaseUpdatePayload');
    expect(page).toContain('releases.update');
    // A second, page-local body builder would make every assertion above a
    // statement about dead code.
    expect(page).not.toMatch(/body:\s*body\.trim\(\)\s*\|\|/);
  });
});
