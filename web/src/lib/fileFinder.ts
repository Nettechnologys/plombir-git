/**
 * "Go to file": the paths that match what was typed, best first, the way
 * GitHub's file finder ranks them. Every typed character has to appear in the
 * path in order, without regard to case; a path scores higher the more of the
 * match falls in the file name, in one run, and the shorter the path.
 */
export interface FileMatch {
	path: string;
	/** Indexes of the matched characters in `path`, for highlighting. */
	positions: number[];
}

function score(path: string, query: string): FileMatch & { score: number } | null {
	const lower = path.toLowerCase();
	const nameStart = lower.lastIndexOf('/') + 1;
	// A contiguous match in the file name beats anything scattered.
	const inName = lower.indexOf(query, nameStart);
	if (inName >= 0) {
		const positions = Array.from({ length: query.length }, (_, i) => inName + i);
		const prefix = inName === nameStart ? 50 : 0;
		return { path, positions, score: 1000 + prefix - path.length };
	}
	const contiguous = lower.indexOf(query);
	if (contiguous >= 0) {
		const positions = Array.from({ length: query.length }, (_, i) => contiguous + i);
		return { path, positions, score: 500 - path.length };
	}
	// Subsequence: matched from the end so the file name is preferred.
	const positions: number[] = [];
	let at = lower.length - 1;
	for (let q = query.length - 1; q >= 0; q -= 1) {
		while (at >= 0 && lower[at] !== query[q]) at -= 1;
		if (at < 0) return null;
		positions.unshift(at);
		at -= 1;
	}
	const inNameCount = positions.filter((p) => p >= nameStart).length;
	let runs = 1;
	for (let i = 1; i < positions.length; i += 1) if (positions[i] !== positions[i - 1] + 1) runs += 1;
	return { path, positions, score: inNameCount * 10 - runs * 5 - path.length / 10 };
}

export function findFiles(paths: readonly string[], query: string, limit = 20): FileMatch[] {
	const needle = query.trim().toLowerCase().replace(/\s+/g, '');
	if (!needle) return paths.slice(0, limit).map((path) => ({ path, positions: [] }));
	const matches: (FileMatch & { score: number })[] = [];
	for (const path of paths) {
		const match = score(path, needle);
		if (match) matches.push(match);
	}
	matches.sort((a, b) => b.score - a.score || (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
	return matches.slice(0, limit).map(({ path, positions }) => ({ path, positions }));
}

/** `path` cut into runs that are and are not part of the match. */
export function highlightRuns(path: string, positions: readonly number[]): { text: string; hit: boolean }[] {
	const hits = new Set(positions);
	const runs: { text: string; hit: boolean }[] = [];
	for (let i = 0; i < path.length; i += 1) {
		const hit = hits.has(i);
		const last = runs[runs.length - 1];
		if (last && last.hit === hit) last.text += path[i];
		else runs.push({ text: path[i], hit });
	}
	return runs;
}
