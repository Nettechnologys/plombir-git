import type { LabelPayload } from './labels';

/** The state the label settings form holds, exactly as typed into the inputs. */
export interface LabelFormState {
  name: string;
  color: string;
  description: string;
}

/**
 * Build the request body for a label create or update.
 *
 * The description is always sent, and an emptied one is sent as `null`. The
 * server tells the two apart on purpose — `UpdateLabelRequest::description` is
 * an `Option<Option<String>>` behind `clearable::double_option`, where absent
 * means "leave it" and `null` means "clear it" — but `description.trim() ||
 * undefined` produced neither: `JSON.stringify` drops an `undefined` value, so
 * clearing a description sent no key at all, read back as "leave it", and
 * answered `200` with the old text still in place.
 *
 * The same body serves create, where `null` and an absent key mean the same
 * thing to a row that has no description yet.
 */
export function buildLabelPayload(form: LabelFormState): LabelPayload {
  return {
    name: form.name.trim(),
    color: form.color,
    description: form.description.trim() || null
  };
}
