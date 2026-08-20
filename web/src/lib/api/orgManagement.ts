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
