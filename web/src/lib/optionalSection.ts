/**
 * A page section that is allowed to be missing, without letting "missing" and
 * "failed" collapse into the same value.
 *
 * A page built out of one required request and several optional ones used to
 * write the optional ones as `.catch(() => null)` / `.catch(() => [])`. That
 * turns a refusal into the empty value the slot would hold anyway, and the
 * markup then states it as a fact about the data: a pull request whose diff
 * endpoint answered 5xx rendered as "No diff available", i.e. a claim about
 * the branch, produced by a git layer that never answered (card_c84bb28a36e1).
 *
 * So a failed optional request resolves to `UNAVAILABLE` instead. The caller
 * has to look at it before it can reach the markup, which is the point: it
 * cannot be rendered as data by accident.
 */
export type Unavailable = { readonly sectionUnavailable: true };

export const UNAVAILABLE: Unavailable = { sectionUnavailable: true };

export function isUnavailable(value: unknown): value is Unavailable {
	return value === UNAVAILABLE;
}

/**
 * Run an optional request, reporting a failure as `UNAVAILABLE` and logging it.
 *
 * `description` is what the reader would call the missing part ("the diff of
 * this pull request"), because it ends up in the console line a developer sees
 * when the page comes back half-empty.
 */
export function optionalSection<T>(
	request: Promise<T>,
	description: string,
): Promise<T | Unavailable> {
	return request.catch((cause: unknown) => {
		console.warn(`Could not load ${description}:`, cause);
		return UNAVAILABLE;
	});
}

/**
 * Unwrap an optional result, recording the name of a section that failed.
 *
 * `fallback` is what the page shows in place of the section — the same empty
 * value as before, but now with the section's name collected in `missing`, so
 * the page can say which parts of it are absent rather than pretending they
 * are empty.
 */
export function sectionOr<T>(
	result: T | Unavailable,
	section: string,
	fallback: T,
	missing: string[],
): T {
	if (isUnavailable(result)) {
		missing.push(section);
		return fallback;
	}
	return result;
}
