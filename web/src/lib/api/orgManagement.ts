import type { OrganizationUpdatePayload, OrganizationVisibility } from './orgs';

export interface OrganizationEditFormState {
  displayName: string;
  description: string;
  visibility: OrganizationVisibility;
}

export function buildOrganizationUpdatePayload(
  form: OrganizationEditFormState,
): OrganizationUpdatePayload {
  return {
    display_name: form.displayName.trim(),
    description: form.description.trim(),
    visibility: form.visibility,
  };
}

export function parseUserId(value: string): number | null {
  const normalized = value.trim();
  if (!/^\d+$/.test(normalized)) return null;

  const userId = Number(normalized);
  return Number.isSafeInteger(userId) && userId > 0 ? userId : null;
}
