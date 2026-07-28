import { defineConfig } from 'vitest/config';

// Deliberately NOT `vite.config.ts`: that one loads the SvelteKit plugin, and
// the unit tests here exercise plain `.ts` modules, so pulling the whole kit
// pipeline in would only add startup cost and failure modes.
//
// `environment: 'jsdom'` is the point of this file. `src/lib/utils/markdown.ts`
// sanitizes untrusted markdown through `DOMParser`, which Node does not have —
// without a DOM the module throws instead of quietly testing something else.
export default defineConfig({
	test: {
		environment: 'jsdom',
		include: ['src/**/*.test.ts']
	}
});
