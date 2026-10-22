import type { RepoTreeEntry } from '$lib/api/repos';

/**
 * Where a repository's README is, looked for the way the forges a repository
 * moves here from look for it (card_68a6f79c5fe4).
 *
 * At the root of a repository Gitea and Forgejo look in `.gitea/`, then
 * `.github/`, then the root itself, then `docs/`
 * (`routers/web/repo/view_readme.go`, `findReadmeFileInRepoTree`); GitHub in
 * `.github/`, the root, then `docs/` — the same order without `.gitea/`. A
 * repository whose README lives in one of those directories used to open here
 * with no README at all.
 */

/** The README names Gitea shows, by preference: Markdown, then `.txt`, then
 *  none. Matched without regard to case, as Gitea and GitHub match them. */
function readmeRank(name: string): number | null {
	const match = /^readme(?:\.(md|markdown|txt))?$/i.exec(name);
	if (!match) return null;
	const extension = match[1]?.toLowerCase();
	if (extension === 'md' || extension === 'markdown') return 0;
	if (extension === 'txt') return 1;
	return 2;
}

/** The README among one directory's entries, or `null`. Of two names of the
 *  same kind the one sorting first wins, so `README.md` is preferred to
 *  `readme.md`. */
export function findReadme(entries: readonly RepoTreeEntry[]): string | null {
	let best: { name: string; rank: number } | null = null;
	for (const entry of entries) {
		if (entry.kind !== 'blob') continue;
		const rank = readmeRank(entry.name);
		if (rank === null) continue;
		if (!best || rank < best.rank || (rank === best.rank && entry.name < best.name)) {
			best = { name: entry.name, rank };
		}
	}
	return best?.name ?? null;
}

/** The directories of the root a README is looked for in, in order; `''` is
 *  the root itself. Only directories the root has are named, under their own
 *  spelling — an exact `.github` is preferred to a `.GitHub`. */
export function readmeDirectories(root: readonly RepoTreeEntry[]): string[] {
	const directory = (wanted: string): string | null => {
		const found = root.filter((entry) => entry.kind === 'tree' && entry.name.toLowerCase() === wanted);
		return (found.find((entry) => entry.name === wanted) ?? found[0])?.name ?? null;
	};
	return [directory('.gitea'), directory('.github'), '', directory('docs')].filter(
		(name): name is string => name !== null,
	);
}

/** Where a relative link of a file in the repository directory `directory`
 *  leads: a path inside the repository, whether it names a directory, and the
 *  fragment it carries. `null` for anything that is not such a link — an
 *  absolute URL, a path of the site, a fragment of the page itself. A `..`
 *  above the root stays at the root, as on GitHub. */
export function repositoryLinkTarget(
	directory: string,
	target: string,
): { path: string; isDirectory: boolean; fragment: string } | null {
	const trimmed = target.trim();
	if (!trimmed || /^[#/?]/.test(trimmed) || /^[a-z][a-z0-9+.-]*:/i.test(trimmed)) return null;
	const end = trimmed.search(/[?#]/);
	const pathPart = end === -1 ? trimmed : trimmed.slice(0, end);
	const hash = trimmed.indexOf('#');
	const fragment = hash === -1 ? '' : trimmed.slice(hash);
	const segments = directory ? directory.split('/') : [];
	for (const raw of pathPart.split('/')) {
		let segment = raw;
		try {
			segment = decodeURIComponent(raw);
		} catch {
			// A stray `%` is part of the name.
		}
		if (segment === '' || segment === '.') continue;
		if (segment === '..') {
			segments.pop();
			continue;
		}
		segments.push(segment);
	}
	const isDirectory = pathPart.endsWith('/') || /(^|\/)\.\.?$/.test(pathPart) || segments.length === 0;
	return { path: segments.join('/'), isDirectory, fragment };
}

/** Where the links and images of a rendered repository file point. */
export interface RepositoryLinkTargets {
	/** The directory of the file, `''` at the root. */
	directory: string;
	/** The page a link to `path` opens: the file's view, or the directory's
	 *  listing. */
	page(path: string, isDirectory: boolean): string;
	/** The address an image stored at `path` loads from. */
	image(path: string): string;
}

/** `html` with its relative links and images pointed into the repository, the
 *  way GitHub and Gitea resolve them against the file's own directory —
 *  `docs/guide.md` from `.github/README.md` is `.github/docs/guide.md`. Every
 *  other address is left as it is. */
export function resolveRepositoryLinks(html: string, targets: RepositoryLinkTargets): string {
	const doc = new DOMParser().parseFromString(html, 'text/html');
	for (const link of Array.from(doc.body.querySelectorAll('a[href]'))) {
		const target = repositoryLinkTarget(targets.directory, link.getAttribute('href') ?? '');
		if (target) link.setAttribute('href', targets.page(target.path, target.isDirectory) + target.fragment);
	}
	for (const image of Array.from(doc.body.querySelectorAll('img[src]'))) {
		const target = repositoryLinkTarget(targets.directory, image.getAttribute('src') ?? '');
		if (target && !target.isDirectory) image.setAttribute('src', targets.image(target.path));
	}
	return doc.body.innerHTML;
}
