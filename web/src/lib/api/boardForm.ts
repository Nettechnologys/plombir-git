import type { BoardCardUpdatePayload, BoardUpdatePayload } from './boards';

export interface BoardEditFormState {
  name: string;
  description: string;
}

export interface BoardCardEditFormState {
  note: string;
  issueId: string;
}

export function buildBoardUpdatePayload(form: BoardEditFormState): BoardUpdatePayload {
  return {
    name: form.name.trim(),
    // The board API stores an empty string when its optional description is
    // emptied. It does not yet expose the three-state clearable contract.
    description: form.description.trim(),
  };
}

export function buildColumnUpdatePayload(name: string): { name: string } {
  return { name: name.trim() };
}

/**
 * Build the card editor PATCH body.
 *
 * The selected link is always present. A blank selection is an explicit
 * `null` detach; a note edit on a linked card carries its existing id instead
 * of accidentally turning the link into an omitted/cleared value.
 */
export function buildBoardCardUpdatePayload(form: BoardCardEditFormState): BoardCardUpdatePayload {
  const rawIssueId = form.issueId.trim();
  const issueId = rawIssueId ? Number(rawIssueId) : null;
  if (issueId !== null && (!Number.isInteger(issueId) || issueId <= 0)) {
    throw new RangeError(`invalid issue id: ${form.issueId}`);
  }

  return {
    note: form.note.trim(),
    issue_id: issueId,
  };
}
