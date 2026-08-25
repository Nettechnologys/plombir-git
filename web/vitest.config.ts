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
		include: ['src/**/*.test.ts'],
		setupFiles: ['src/lib/test/setup.ts'],
	}
});
