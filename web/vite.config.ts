import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// The app calls its backend on the origin it was loaded from: `_base.svelte.ts`
// defaults the API base to the relative `/api/v1`, and the auth cookie is
// HttpOnly + SameSite=Strict, so a cross-origin frontend would not send it at
// all. Both dev servers therefore have to proxy the same paths to the same
// backend.
//
// `preview` used to have no proxy, which made `vite preview` unusable for an
// end-to-end run: every page rendered, every API call hit the preview server's
// own 404, and the failure looked like a broken app rather than a missing
// proxy. `scripts/lib/stand.sh` serves the built app this way and binds its
// backend to port 0, so the target cannot be a literal here — it
// comes in through PLOMBIR_GIT_BACKEND_ORIGIN, defaulting to the documented dev
// port so `npm run dev` keeps working with nothing set.
// This file is the one module of the frontend that runs in Node rather than in
// the browser, and the project ships no `@types/node`. Declaring the single
// global it needs here — instead of adding the package, or `types: ["node"]` to
// the tsconfig — keeps Node's globals out of `src/`, where `process.env` or
// `Buffer` type-checking clean would be a bug the checker is supposed to catch.
declare const process: { env: Record<string, string | undefined> };

const backendOrigin = process.env.PLOMBIR_GIT_BACKEND_ORIGIN || 'http://127.0.0.1:8080';

// `ws: true` on the API prefix is what carries the notification socket
// (`/api/v1/ws/notifications`, `web/src/lib/api/websockets.ts`): without the
// upgrade the browser retries a socket that can never connect and the console
// fills with errors a smoke test then reports as the app's fault.
const proxy = {
	'/api/v1': { target: backendOrigin, changeOrigin: true, ws: true },
	'/health': { target: backendOrigin, changeOrigin: true }
};

export default defineConfig({
	plugins: [sveltekit()],
	server: { proxy },
	preview: { proxy }
});
