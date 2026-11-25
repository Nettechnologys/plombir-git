import { describe, expect, it } from 'vitest';

import type { RepoTreeEntry } from './api/repos';
import { sortTreeEntries } from './repoTree';

const entry = (name: string, kind: RepoTreeEntry['kind']): RepoTreeEntry => ({ name, kind });

describe('sortTreeEntries', () => {
	it('lists directories and submodules before files, each alphabetically without regard to case', () => {
		const gitOrder = [
			entry('.dockerignore', 'blob'),
			entry('.github', 'tree'),
			entry('CHANGELOG.md', 'blob'),
			entry('README.md', 'blob'),
			entry('Vendor', 'commit'),
			entry('docs', 'tree'),
			entry('package.json', 'blob'),
			entry('src', 'tree'),
		];
		expect(sortTreeEntries(gitOrder).map((e) => e.name)).toEqual([
			'.github',
			'docs',
			'src',
			'Vendor',
			'.dockerignore',
			'CHANGELOG.md',
			'package.json',
			'README.md',
		]);
	});

	it('orders names that differ only in case deterministically and leaves the input untouched', () => {
		const input = [entry('readme', 'blob'), entry('README', 'blob')];
		expect(sortTreeEntries(input).map((e) => e.name)).toEqual(['README', 'readme']);
		expect(input.map((e) => e.name)).toEqual(['readme', 'README']);
	});
});
