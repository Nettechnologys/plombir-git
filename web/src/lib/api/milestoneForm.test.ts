import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import MilestonesPage from '../../routes/[owner]/[repo]/milestones/+page.svelte';
import {
  buildMilestoneCreatePayload,
  buildMilestoneUpdatePayload,
  dueDateForInput,
  type MilestoneFormState,
} from './milestoneForm';
import { setTestPage } from '../test/app';
import { milestones, resetTestClient } from '../test/client';
import { button, click, element, input, renderComponent, submit, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const milestone = {
	id: 7,
	title: 'v1.0',
	description: 'First stable release',
	due_date: '2030-01-01T00:00:00Z',
	state: 'open',
};

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/milestones', { owner: 'alice', repo: 'demo' });
	milestones.list.mockResolvedValue([milestone]);
	milestones.get.mockResolvedValue(milestone);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

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

	it('submits explicit clears from the rendered milestone editor', async () => {
		rendered = await renderComponent(MilestonesPage);
		await click(button(rendered.container, 'Edit'));
		await input(element(rendered.container, '.editor textarea'), '   ');
		await input(element(rendered.container, '.editor input[type="date"]'), '');
		await submit(element(rendered.container, '.editor form'));

		expect(milestones.update).toHaveBeenCalledWith('alice', 'demo', 7, {
			title: 'v1.0',
			description: null,
			due_date: null,
			state: 'open',
		});
	});
});
