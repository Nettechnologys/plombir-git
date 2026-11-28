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

/**
 * Whether assigning the issue as `form` says hands an agent text it would not
 * trust otherwise: the chosen assignee is a bot, the choice is a new one, and
 * the issue's author cannot write to the repository (ADR-0001 D7). An agent
 * run reads the assigned issue's body whoever wrote it — the person assigning
 * vouches for it — so the page says so before they do.
 */
export function agentReadsUntrustedAuthor(
  form: IssueLinksFormState,
  currentAssigneeId: number | null,
  botIds: ReadonlySet<number>,
  authorCanWrite: boolean | undefined,
): boolean {
  const chosen = Number(form.assigneeId.trim());
  if (!form.assigneeId.trim() || !Number.isInteger(chosen)) return false;
  return chosen !== currentAssigneeId && botIds.has(chosen) && authorCanWrite === false;
}

/** Build the explicit set/clear body used only by the issue links form. */
export function buildIssueLinksPayload(form: IssueLinksFormState): IssueUpdatePayload {
  return {
    assignee_id: nullableId(form.assigneeId),
    milestone_id: nullableId(form.milestoneId),
  };
}
