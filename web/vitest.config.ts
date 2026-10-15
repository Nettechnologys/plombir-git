import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

// `environment: 'jsdom'` is the point of this file. `src/lib/utils/markdown.ts`
// sanitizes untrusted markdown through `DOMParser`, which Node does not have —
// without a DOM the module throws instead of quietly testing something else.
//
// Route and component tests also need the SvelteKit transform. Keeping the
// plugin here (rather than importing `vite.config.ts`) avoids inheriting the
// development proxy while still compiling `.svelte` files and resolving Kit's
// virtual `$app/*` modules.
export default defineConfig({
	plugins: [sveltekit()],
	resolve: {
		conditions: ['browser'],
	},
	test: {
		environment: 'jsdom',
		// Node 25 and later define a global `localStorage` of their own, which
		// shadows the one jsdom installs: every test touching storage then reads
		// `undefined`. The image builds the frontend on Node 26, so the tests run
		// with Node's Web Storage switched off and jsdom's in its place.
		execArgv: ['--no-experimental-webstorage'],
		include: ['src/**/*.test.ts'],
		setupFiles: ['src/lib/test/setup.ts'],
	}
});
