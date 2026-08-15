import { describe, expect, it } from 'vitest';

import milestonePageSource from '../../routes/[owner]/[repo]/milestones/+page.svelte?raw';
import {
  buildMilestoneCreatePayload,
  buildMilestoneUpdatePayload,
  dueDateForInput,
  type MilestoneFormState,
} from './milestoneForm';

function formState(overrides: Partial<MilestoneFormState> = {}): MilestoneFormState {
  return {
    title: 'v1.0',
    description: 'First stable release',
    dueDate: '2030-01-01',
    state: 'open',
    ...overrides,
  };
}

function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('milestone form payloads', () => {
  it('sends blank update fields as explicit nulls so both stored values clear', () => {
    const body = wire(buildMilestoneUpdatePayload(formState({ description: ' ', dueDate: '' })));

    expect(body).toHaveProperty('description', null);
    expect(body).toHaveProperty('due_date', null);
  });

  it('keeps create payloads compact and converts dates to RFC 3339', () => {
    expect(wire(buildMilestoneCreatePayload(formState({ description: ' ', dueDate: '' })))).toEqual({
      title: 'v1.0',
      state: 'open',
    });
    expect(buildMilestoneCreatePayload(formState()).due_date).toBe('2030-01-01T00:00:00Z');
    expect(dueDateForInput('2030-01-01T00:00:00Z')).toBe('2030-01-01');
  });

  it('is wired into both create and update calls on the milestone page', () => {
    expect(milestonePageSource).toContain('buildMilestoneCreatePayload');
    expect(milestonePageSource).toContain('buildMilestoneUpdatePayload');
    expect(milestonePageSource).toContain('milestones.create');
    expect(milestonePageSource).toContain('milestones.update');
    expect(milestonePageSource).not.toMatch(/description:\s*form\.description/);
  });
});
