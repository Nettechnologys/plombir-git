import { describe, expect, it } from 'vitest';

import { renderMarkdown, sanitizeHtml } from './markdown';

// The tags `sanitizeHtml` is allowed to emit, restated here on purpose. If the
// test imported the allowlist from the module it would assert "the sanitizer
// agrees with itself"; written out, it asserts what the DOM is allowed to
// receive, and widening the allowlist has to be a deliberate edit in two places.
const ALLOWED_TAGS = new Set([
	'a',
	'blockquote',
	'br',
	'code',
	'del',
	'em',
	'h1',
	'h2',
	'h3',
	'h4',
	'h5',
	'h6',
	'hr',
	'img',
	'li',
	'ol',
	'p',
	'pre',
	'strong',
	'table',
	'tbody',
	'td',
	'th',
	'thead',
	'tr',
	'ul'
]);

const SAFE_SCHEMES = new Set(['http:', 'https:', 'mailto:']);

/**
 * The invariant, asserted over a parsed tree rather than over the output string.
 * String matching only proves that the payloads we happened to think of are
 * absent — and it produces false alarms, because `onerror=` sitting inside a
 * text node or an attribute *value* is inert. What matters is what the parser
 * builds: no tag outside the allowlist, no event handler, no `style`, no URL
 * outside http/https/mailto.
 */
function expectInertTree(root: Element, label: string) {
	const walker = root.ownerDocument.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);

	while (walker.nextNode()) {
		const element = walker.currentNode as Element;
		const tagName = element.tagName.toLowerCase();
		expect(ALLOWED_TAGS, `tag <${tagName}> survived in ${label}`).toContain(tagName);

		for (const attr of Array.from(element.attributes)) {
			const name = attr.name.toLowerCase();
			expect(name.startsWith('on'), `attribute ${name} survived in ${label}`).toBe(false);
			expect(name, `attribute ${name} survived in ${label}`).not.toBe('style');

			if (name === 'href' || name === 'src') {
				const resolved = new URL(attr.value, 'https://plombir-git.local');
				expect(SAFE_SCHEMES, `${name}="${attr.value}" survived in ${label}`).toContain(
					resolved.protocol
				);
			}
		}
	}
}

function expectInertMarkup(html: string) {
	// Parse #1: how the sanitizer itself sees the markup.
	const doc = new DOMParser().parseFromString(html, 'text/html');
	expectInertTree(doc.body, `parsed output of: ${html}`);

	// Parse #2: how the app actually consumes it. `{@html}` assigns to innerHTML
	// on a live document, which is a different insertion mode than DOMParser —
	// and that gap is exactly what mXSS payloads are built to exploit.
	//
	// Known limit, stated rather than papered over: jsdom parses with scripting
	// disabled, so `<noscript>` content becomes markup here where a real browser
	// (scripting enabled) would treat it as raw text. It cannot produce a false
	// green for us — `noscript` is not in the allowlist, so it never survives
	// into the output — but a payload class that needs scripting-enabled parsing
	// to trigger would need a real browser to reproduce.
	const host = document.createElement('div');
	host.innerHTML = html;
	expectInertTree(host, `live insertion of: ${html}`);
}

/**
 * Every result must be a fixpoint. The browser parses our output a second time;
 * if a second sanitize pass changes it, the two parses disagreed and the markup
 * we shipped is not the markup we vetted.
 */
function expectStable(html: string) {
	expect(sanitizeHtml(html), `not a fixpoint: ${html}`).toBe(html);
}

function sanitized(payload: string): string {
	const out = sanitizeHtml(payload);
	expectInertMarkup(out);
	expectStable(out);
	return out;
}

describe('sanitizeHtml runs the implementation the browser runs', () => {
	// The regression this whole suite exists for: the previous gate loaded the
	// module in bare Node, where `DOMParser` is absent, so it asserted a regex
	// fallback that no browser ever executed while the real path went untested.
	it('exercises the DOMParser path, not a substitute', () => {
		expect(typeof DOMParser).not.toBe('undefined');
		expect(typeof NodeFilter).not.toBe('undefined');
	});

	it('refuses to sanitize without a DOM instead of degrading to a weaker branch', () => {
		const saved = Reflect.get(globalThis, 'DOMParser');
		Reflect.deleteProperty(globalThis, 'DOMParser');
		try {
			expect(() => sanitizeHtml('<p>x</p>')).toThrow(/requires a DOM/);
		} finally {
			Reflect.set(globalThis, 'DOMParser', saved);
		}
		// Guard against the restore silently failing and poisoning later tests.
		expect(sanitizeHtml('<p>x</p>')).toBe('<p>x</p>');
	});
});

describe('sanitizeHtml rejects script-bearing URLs', () => {
	const payloads: Record<string, string> = {
		plain: '<a href="javascript:alert(1)">x</a>',
		'mixed case': '<a href="JaVaScRiPt:alert(1)">x</a>',
		'leading whitespace': '<a href="   javascript:alert(1)">x</a>',
		// The bypass found in the deleted regex branch: it validated the raw
		// attribute text, so the colon hidden in an entity read as a relative path.
		'entity colon, decimal': '<a href="javascript&#58;alert(1)">x</a>',
		'entity colon, hex': '<a href="javascript&#x3a;alert(1)">x</a>',
		'entity first letter': '<a href="&#106;avascript:alert(1)">x</a>',
		'embedded tab': '<a href="java&#09;script:alert(1)">x</a>',
		'embedded newline': '<a href="java&#10;script:alert(1)">x</a>',
		'embedded carriage return': '<a href="java&#13;script:alert(1)">x</a>',
		'leading control character': '<a href="&#01;javascript:alert(1)">x</a>',
		'on img src': '<img src="javascript:alert(1)">',
		'data url': '<a href="data:text/html,<script>alert(1)</script>">x</a>',
		'vbscript url': '<a href="vbscript:msgbox(1)">x</a>'
	};

	for (const [name, payload] of Object.entries(payloads)) {
		it(`strips the URL attribute: ${name}`, () => {
			const out = sanitized(payload);
			expect(out).not.toMatch(/href=/i);
			expect(out).not.toMatch(/src=/i);
			expect(out.toLowerCase()).not.toContain('javascript');
		});
	}

	it('strips it through the markdown pipeline too', () => {
		const out = renderMarkdown('[x](javascript:alert\\(1\\))');
		expectInertMarkup(out);
		expect(out).not.toMatch(/href=/i);
		expect(out.toLowerCase()).not.toContain('javascript');
	});
});

describe('sanitizeHtml removes event handlers', () => {
	const payloads: Record<string, string> = {
		onerror: '<p><img src="/logo.png" onerror="alert(1)"></p>',
		onclick: '<a href="https://example.com" onclick="alert(1)">safe</a>',
		'uppercase ONERROR': '<img src="/logo.png" ONERROR="alert(1)">',
		unquoted: '<img src=/logo.png onerror=alert(1)>',
		'on an unwrapped tag': '<div onmouseover="alert(1)"><strong>ok</strong></div>',
		onfocus: '<a href="#x" onfocus="alert(1)" autofocus>x</a>'
	};

	for (const [name, payload] of Object.entries(payloads)) {
		it(`drops the handler: ${name}`, () => {
			const out = sanitized(payload);
			expect(out.toLowerCase()).not.toContain('alert(1)');
		});
	}
});

describe('sanitizeHtml drops tags outside the allowlist', () => {
	const payloads: Record<string, string> = {
		script: '<script>alert(1)</script><strong>ok</strong>',
		'svg with script': '<svg><script>alert(1)</script></svg><strong>ok</strong>',
		math: '<math><mtext>x</mtext></math><strong>ok</strong>',
		iframe: '<iframe src="https://evil.example"></iframe><strong>ok</strong>',
		object: '<object data="https://evil.example"></object><strong>ok</strong>',
		embed: '<embed src="https://evil.example"><strong>ok</strong>',
		base: '<base href="https://evil.example"><strong>ok</strong>',
		form: '<form action="https://evil.example"><strong>ok</strong></form>',
		style: '<style>body{background:url(javascript:alert(1))}</style><strong>ok</strong>',
		'style attribute': '<p style="background:url(javascript:alert(1))"><strong>ok</strong></p>'
	};

	for (const [name, payload] of Object.entries(payloads)) {
		it(`neutralizes: ${name}`, () => {
			const out = sanitized(payload);
			expect(out).toContain('<strong>ok</strong>');
			expect(out.toLowerCase()).not.toContain('<script');
			expect(out.toLowerCase()).not.toContain('<svg');
			expect(out.toLowerCase()).not.toContain('<iframe');
		});
	}
});

describe('sanitizeHtml survives markup that reinterprets itself (mXSS)', () => {
	// Unwrapping `<svg>`/`<math>` moves their children from foreign content into
	// HTML context, where the same bytes can parse differently. These are the
	// published shapes of that attack against unwrap-based sanitizers.
	const payloads: Record<string, string> = {
		'svg style breakout':
			'<svg></p><style><a id="</style><img src=1 onerror=alert(1)>"></style></p></svg>',
		'math mglyph style':
			'<math><mtext><table><mglyph><style><!--</style><img title="--><img src=x onerror=alert(1)>">',
		'svg raw text': '<svg><style><img src=x onerror=alert(1)></style></svg>',
		'noscript breakout': '<noscript><p title="</noscript><img src=x onerror=alert(1)>"></noscript>',
		'form mglyph': '<form><math><mtext></form><form><mglyph><style></math><img src onerror=alert(1)>',
		'nested comment': '<svg><!--</svg><img src=x onerror=alert(1)>-->',
		'textarea breakout': '<textarea><p title="</textarea><img src=x onerror=alert(1)>">'
	};

	for (const [name, payload] of Object.entries(payloads)) {
		it(`stays inert after a second parse: ${name}`, () => {
			// `sanitized()` is the whole assertion here, and it is a strong one:
			// the output is re-parsed twice (DOMParser and live `innerHTML`,
			// the two insertion modes that disagree in an mXSS) and required to
			// be a sanitizer fixpoint. A payload that reinterprets itself fails
			// at least one of the three.
			//
			// Not asserted on the string: several of these leave `onerror=` text
			// behind inside an attribute *value* (`<p title="...onerror=...">`)
			// or a text node, where it is inert. Grepping the output would flag
			// those and miss a handler smuggled in through an unusual tag name —
			// which is why the check reads the tree the parser built instead.
			sanitized(payload);
		});
	}
});

describe('sanitizeHtml keeps legitimate markup', () => {
	it('keeps a safe link and marks it up', () => {
		const out = sanitized('<a href="https://example.com" title="t">safe</a>');
		expect(out).toContain('href="https://example.com"');
		expect(out).toContain('rel="nofollow noopener noreferrer"');
		expect(out).toContain('title="t"');
	});

	it('keeps relative, anchor and mailto targets', () => {
		expect(sanitized('<a href="/repo/issues">x</a>')).toContain('href="/repo/issues"');
		expect(sanitized('<a href="#section">x</a>')).toContain('href="#section"');
		expect(sanitized('<a href="./rel">x</a>')).toContain('href="./rel"');
		expect(sanitized('<a href="mailto:a@b.example">x</a>')).toContain('href="mailto:a@b.example"');
	});

	it('keeps images, code classes and table alignment', () => {
		expect(sanitized('<p><img src="/logo.png" alt="logo"></p>')).toContain('src="/logo.png"');
		expect(sanitized('<pre><code class="language-rust">fn main(){}</code></pre>')).toContain(
			'class="language-rust"'
		);
		expect(sanitized('<table><tbody><tr><td align="right">1</td></tr></tbody></table>')).toContain(
			'align="right"'
		);
	});

	it('escapes text instead of dropping it when unwrapping', () => {
		const out = sanitized('<div>kept text</div>');
		expect(out).toContain('kept text');
	});

	it('renders ordinary markdown end to end', () => {
		const out = renderMarkdown('# Title\n\nSome **bold** and `code` and [a link](https://example.com).');
		expectInertMarkup(out);
		expect(out).toContain('<h1>Title</h1>');
		expect(out).toContain('<strong>bold</strong>');
		expect(out).toContain('<code>code</code>');
		expect(out).toContain('href="https://example.com"');
	});
});
