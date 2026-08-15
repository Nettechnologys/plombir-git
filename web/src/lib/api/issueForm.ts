import type { IssueUpdatePayload } from './issues';

export interface IssueLinksFormState {
  assigneeId: string;
  milestoneId: string;
}

function nullableId(value: string): number | null {
  const normalized = value.trim();
  if (!normalized) return null;

  const id = Number(normalized);
  if (!Number.isInteger(id) || id <= 0) {
    throw new RangeError(`invalid resource id: ${value}`);
  }
  return id;
}

/** Build the explicit set/clear body used only by the issue links form. */
export function buildIssueLinksPayload(form: IssueLinksFormState): IssueUpdatePayload {
  return {
    assignee_id: nullableId(form.assigneeId),
    milestone_id: nullableId(form.milestoneId),
  };
}
