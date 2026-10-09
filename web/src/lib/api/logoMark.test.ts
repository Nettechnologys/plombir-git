import { afterEach, describe, expect, it } from 'vitest';
// This test reads the checked-out sources and static assets.
// @ts-expect-error Node declarations are intentionally absent from the app.
import { existsSync, readFileSync, readdirSync } from 'node:fs';
// @ts-expect-error Node declarations are intentionally absent from the app.
import { join } from 'node:path';

declare const process: { cwd(): string };

import HelpPage from '../../routes/help/+page.svelte';
import { renderComponent, type RenderedComponent } from '../test/render';

// card_e34af7255aa6: the product wore GitHub's Octocat, had no favicon or
// manifest, promised figures nobody measured, and taught cloning from
// localhost.

const web = process.cwd();

function sources(dir: string): string[] {
	return readdirSync(dir, { withFileTypes: true }).flatMap((entry: any) =>
		entry.isDirectory()
			? sources(join(dir, entry.name))
			: /\.(svelte|ts|html|css)$/.test(entry.name) && !entry.name.endsWith('.test.ts')
				? [join(dir, entry.name)]
				: [],
	);
}

function paths(svg: string): string[] {
	return Array.from(svg.matchAll(/\bd="([^"]+)"/g), (match) => match[1]);
}

let rendered: RenderedComponent | undefined;

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('the mark', () => {
	it('is nobody else’s', () => {
		const octocat = 'M8 0C3.58 0 0 3.58 0 8';
		const carriers = sources(join(web, 'src')).filter((file) => readFileSync(file, 'utf8').includes(octocat));
		expect(carriers).toEqual([]);
	});

	it('is the same drawing in the app and in the favicon', () => {
		const component = readFileSync(join(web, 'src/lib/components/Logo.svelte'), 'utf8');
		const favicon = readFileSync(join(web, 'static/favicon.svg'), 'utf8');
		expect(paths(component).length).toBeGreaterThan(0);
		expect(paths(favicon)).toEqual(paths(component));
	});

	it('is linked as the icon, the touch icon and the manifest', () => {
		const app = readFileSync(join(web, 'src/app.html'), 'utf8');
		expect(app).toContain('rel="icon" href="%sveltekit.assets%/favicon.svg"');
		expect(app).toContain('rel="apple-touch-icon" href="%sveltekit.assets%/apple-touch-icon.png"');
		expect(app).toContain('rel="manifest" href="%sveltekit.assets%/manifest.webmanifest"');
		expect(existsSync(join(web, 'static/apple-touch-icon.png'))).toBe(true);
		const manifest = JSON.parse(readFileSync(join(web, 'static/manifest.webmanifest'), 'utf8'));
		expect(manifest.name).toBe('Plombir Git');
		for (const icon of manifest.icons) {
			expect(existsSync(join(web, 'static', icon.src))).toBe(true);
		}
	});
});

describe('the landing page', () => {
	it('states no figure nobody measured', () => {
		const page = readFileSync(join(web, 'src/routes/+page.svelte'), 'utf8');
		const markup = page
			.replace(/<!--[\s\S]*?-->/g, '')
			.replace(/<style>[\s\S]*?<\/style>/g, '')
			.replace(/<script[\s\S]*?<\/script>/g, '');
		const en = readFileSync(join(web, 'src/lib/i18n/translations/en.json'), 'utf8');
		for (const claim of [/\b\d+\s?MB\b/, /100%/]) {
			expect(markup).not.toMatch(claim);
			expect(JSON.stringify(JSON.parse(en).home)).not.toMatch(claim);
		}
	});
});

describe('the help page', () => {
	it('teaches cloning from this instance, not from localhost', async () => {
		rendered = await renderComponent(HelpPage);
		const commands = Array.from(rendered.container.querySelectorAll('.command-list code'), (code) => code.textContent);
		expect(commands[0]).toBe(`git clone ${window.location.origin}/git/OWNER/REPO`);
		expect(commands.join('\n')).not.toContain('localhost:8080');
	});
});
