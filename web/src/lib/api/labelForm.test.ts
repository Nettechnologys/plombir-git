import { describe, expect, it } from 'vitest';

// The page's own source, pulled in by vite so the check below needs no node
// filesystem API (and no `@types/node` for `npm run check`).
import settingsPageSource from '../../routes/[owner]/[repo]/settings/labels/+page.svelte?raw';
import { buildLabelPayload, type LabelFormState } from './labelForm';

function formState(overrides: Partial<LabelFormState> = {}): LabelFormState {
  return { name: 'bug', color: '#ff0000', description: 'Something is broken', ...overrides };
}

/** What the network actually carries — `JSON.stringify` drops an `undefined` value. */
function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('buildLabelPayload', () => {
  it('sends an emptied description as null, so it can actually be cleared', () => {
    const body = wire(buildLabelPayload(formState({ description: '   ' })));

    // `null` is the API's "clear it"; a dropped key is its "leave it alone",
    // and a dropped key is what an emptied field used to become.
    expect(body).toHaveProperty('description');
    expect(body.description).toBeNull();
  });

  it('drops no key on the way to the wire, whatever the form holds', () => {
    const body = wire(buildLabelPayload(formState({ name: '', description: '' })));

    expect(Object.keys(body).sort()).toEqual(['color', 'description', 'name']);
  });

  it('still carries what the operator typed', () => {
    const body = wire(buildLabelPayload(formState({ name: '  wontfix  ' })));

    expect(body).toEqual({
      name: 'wontfix',
      color: '#ff0000',
      description: 'Something is broken'
    });
  });

  it('is the builder the settings form itself uses, on both create and update', () => {
    const page = settingsPageSource;

    expect(page).toContain('buildLabelPayload');
    expect(page).toContain('labels.update');
    expect(page).toContain('labels.create');
    // A second, page-local body builder would make every assertion above a
    // statement about dead code.
    expect(page).not.toMatch(/description:\s*formData\.description/);
  });
});
