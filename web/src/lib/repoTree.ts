import type { RepoTreeEntry } from '$lib/api/repos';

/**
 * A directory listing in the order GitHub shows it: directories and
 * submodules first, then files, each group alphabetical without regard to
 * case. Git itself stores a tree in byte order, which put `CHANGELOG.md` and
 * `.gitignore` above `src/`.
 */
export function sortTreeEntries<T extends RepoTreeEntry>(entries: readonly T[]): T[] {
	return [...entries].sort((a, b) => {
		const group = Number(a.kind === 'blob') - Number(b.kind === 'blob');
		if (group !== 0) return group;
		const left = a.name.toLowerCase();
		const right = b.name.toLowerCase();
		if (left !== right) return left < right ? -1 : 1;
		// Names that differ only in case keep a stable, byte-wise order.
		return a.name < b.name ? -1 : a.name > b.name ? 1 : 0;
	});
}
