import adapter from '@sveltejs/adapter-static';

// SvelteKit 3 no longer reads `svelte.config.js`: its options go to the
// `sveltekit(...)` Vite plugin, and the build refuses to start while that file
// exists. Both `vite.config.ts` and `vitest.config.ts` call the plugin, and the
// tests must compile the app with the same aliases, adapter and rune mode as the
// build, so the options live here once instead of in two copies that drift.
/** @type {import('@sveltejs/kit/vite').Config} */
const config = {
	compilerOptions: {
		runes: (/** @type {{ filename: string }} */ { filename }) => (filename.split(/[/\\]/).includes('node_modules') ? undefined : true)
	},
	adapter: adapter({
		pages: 'build',
		assets: 'build',
		fallback: 'index.html',
		precompress: false,
		strict: true
	}),
	// Kit 3 dropped the built-in `$lib` module in favour of the `#lib` subpath
	// import. Six hundred imports and the contract checks under `scripts/` that
	// pin them spell `$lib/…`, so the alias keeps them resolving until they move
	// over in one change of their own. (`$api` and `$components` had no users.)
	alias: {
		'$lib': 'src/lib'
	}
};

export default config;
