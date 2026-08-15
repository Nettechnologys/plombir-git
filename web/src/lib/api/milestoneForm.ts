import type { CreateMilestonePayload, Milestone, UpdateMilestonePayload } from './milestones';

export interface MilestoneFormState {
  title: string;
  description: string;
  dueDate: string;
  state: Milestone['state'];
}

/** Convert an HTML date input to the RFC 3339 value accepted by the API. */
function dueDateForApi(value: string): string | null {
  const date = value.trim();
  return date ? `${date}T00:00:00Z` : null;
}

/** Convert a stored RFC 3339 timestamp back to an HTML date input value. */
export function dueDateForInput(value: string | null): string {
  return value ? value.slice(0, 10) : '';
}

export function buildMilestoneCreatePayload(form: MilestoneFormState): CreateMilestonePayload {
  const dueDate = dueDateForApi(form.dueDate);
  return {
    title: form.title.trim(),
    description: form.description.trim() || undefined,
    due_date: dueDate || undefined,
    state: form.state,
  };
}

/**
 * Build a complete milestone PATCH body.
 *
 * Blank clearable fields deliberately become `null`: the server treats a
 * missing key as "leave the stored value alone", so `undefined` would turn a
 * successful-looking save into a no-op.
 */
export function buildMilestoneUpdatePayload(form: MilestoneFormState): UpdateMilestonePayload {
  return {
    title: form.title.trim(),
    description: form.description.trim() || null,
    due_date: dueDateForApi(form.dueDate),
    state: form.state,
  };
}
