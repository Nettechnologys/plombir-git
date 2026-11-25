import { describe, expect, it } from 'vitest';

import { findFiles, highlightRuns } from './fileFinder';

const PATHS = [
	'README.md',
	'crates/rg-http/src/api/repo_content.rs',
	'crates/rg-http/src/api/repo_overview.rs',
	'web/src/routes/[owner]/[repo]/+page.svelte',
	'docs/overview.md',
];

describe('findFiles', () => {
	it('prefers a match in the file name, then a shorter path', () => {
		expect(findFiles(PATHS, 'overview').map((m) => m.path)).toEqual([
			'docs/overview.md',
			'crates/rg-http/src/api/repo_overview.rs',
		]);
	});

	it('matches typed characters in order across the path without regard to case', () => {
		const [match] = findFiles(PATHS, 'RGcontent');
		expect(match.path).toBe('crates/rg-http/src/api/repo_content.rs');
		expect(findFiles(PATHS, 'zzz')).toEqual([]);
	});

	it('lists paths unfiltered for an empty query, up to the limit', () => {
		expect(findFiles(PATHS, '  ', 2).map((m) => m.path)).toEqual(PATHS.slice(0, 2));
	});
});

describe('highlightRuns', () => {
	it('cuts a path into matched and unmatched runs', () => {
		expect(highlightRuns('abcd', [1, 2])).toEqual([
			{ text: 'a', hit: false },
			{ text: 'bc', hit: true },
			{ text: 'd', hit: false },
		]);
	});
});
