#!/usr/bin/env node

// Fixture for the frontend production views behind every `.ts` / `.svelte`
// assertion in `scripts/`.
//
// These are the forms that have produced false green verdicts, or that would
// produce false red ones if the views over-reached. The gate this file guards
// is only as good as the view underneath it: a check rewritten onto
// `productionTsSource` still passes vacuously if the view hands back the raw
// bytes, and that is indistinguishable from a correct run by the check's own
// output (card_a54b6a2db9f2).

import { productionTsCode, productionTsSource, isSvelteComponent, tsFunctionBody, tsInterfaceBody } from './lib/ts-source.mjs';

const failures = [];

function expect(condition, message) {
  if (!condition) failures.push(message);
}

const MODULE = `// packages.publish must set the attachment header.
import { request } from './_base.svelte.ts';

/* headers['Content-Disposition'] = blockCommented(filename); */

export interface PublishResponse {
  id: string;
  // filename: string;
}

export const packages = {
  publish: (body: Blob) => {
    const headers: Record<string, string> = {};
    // headers['Content-Disposition'] = lineCommented(filename);
    headers['Content-Disposition'] = contentDispositionAttachment(filename);
    const marker = 'interface PublishResponse { spelled: string }';
    const pattern = /'/;
    return request<PublishResponse>('/publish', { headers, marker, pattern });
  },
};
`;

// A module is JavaScript at its top level, whatever `<script>` its strings
// spell out — `markdown.ts` carries exactly that in its sanitizer blocklist.
expect(!isSvelteComponent(MODULE), 'a .ts module must not be read as a Svelte component');
expect(
  !isSvelteComponent("const blocked = ['<script>', '<style>'];\n"),
  'a `<script>` inside a string literal must not turn a module into a component',
);

const moduleSource = productionTsSource(MODULE);
const moduleCode = productionTsCode(MODULE);

expect(moduleSource.length === MODULE.length, 'productionTsSource must stay byte-aligned with its input');
expect(moduleCode.length === MODULE.length, 'productionTsCode must stay byte-aligned with its input');

// The defect itself: the live line survives, both commented-out copies do not.
expect(
  /headers\['Content-Disposition'\] = contentDispositionAttachment\(filename\)/.test(moduleSource),
  'the live assignment must survive the production view',
);
expect(
  !/lineCommented/.test(moduleSource),
  'a line-commented assignment must read as deleted',
);
expect(
  !/blockCommented/.test(moduleSource),
  'a block-commented assignment must read as deleted',
);

// Values live in the string-bearing view and are gone from the code-only one.
expect(/'\/publish'/.test(moduleSource), 'productionTsSource must keep string literals');
expect(!/publish'/.test(moduleCode.slice(moduleCode.indexOf('return request'))), 'productionTsCode must blank string bodies');

// A regex literal is lexed, not guessed at: `/'/` opens no string, so the code
// after it is still read as code rather than swallowed to the next quote.
expect(/return request/.test(moduleCode), 'a regex literal must not swallow the code that follows it');

// The interface reader answers about the declaration, not about a later one and
// not about a string that spells its header.
const publishResponse = tsInterfaceBody(MODULE, 'PublishResponse');
expect(publishResponse !== null, 'tsInterfaceBody must read a top-level interface');
expect(publishResponse !== null && /\bid: string/.test(publishResponse), 'tsInterfaceBody must return the live members');
expect(
  publishResponse !== null && !/filename/.test(publishResponse),
  'a commented-out member must not satisfy an assertion about the shape the client sends',
);
expect(
  tsInterfaceBody(MODULE, 'NeverDeclared') === null,
  'tsInterfaceBody must fail loudly on an interface that does not exist',
);
expect(
  tsInterfaceBody('const marker = "interface Ghost {";\nconst x = 1;\n', 'Ghost') === null,
  'an interface header spelled inside a string literal must not be read as a declaration',
);

// The three vectors `card_0bb857f57323` reproduced against the raw-text
// finders, each latent then and each closed here. They are three fixtures
// rather than one because a decoy that wins hides the decoys behind it: with a
// block-commented declaration first in the file, nothing proves the template
// literal would have been rejected too.
const commentDecoy = [
  '/*',
  'export interface Ghost {',
  '  fromComment: string;',
  '}',
  'function readPolicy(value) {',
  '  return true;',
  '}',
  '*/',
  'export interface Ghost {',
  '  live: string;',
  '}',
  'function readPolicy(value) {',
  '  if (value.length < 8) return false;',
  '  return true;',
  '}',
  '',
].join('\n');

const templateDecoy = [
  'const TEMPLATE_DECOY = `',
  'export interface Ghost {',
  '  fromTemplate: string;',
  '}',
  '`;',
  'export interface Ghost {',
  '  live: string;',
  '}',
  '',
].join('\n');

// A real newline followed by `}` inside a template literal — the boundary both
// finders look for. Written with actual line breaks, because an escaped `\\n`
// in a single-quoted string is two characters and would not reproduce it.
const braceDecoy = [
  'export interface Ghost {',
  '  sample: string;',
  '  live: string;',
  '}',
  '',
  'function readPolicy(value) {',
  '  const closer = `',
  '}`;',
  '  if (value.length < 8) return false;',
  '  return closer !== null;',
  '}',
  '',
].join('\n');

for (const [label, fixture, ghostDecoy] of [
  ['a block comment', commentDecoy, /fromComment/],
  ['a template literal', templateDecoy, /fromTemplate/],
]) {
  const ghost = tsInterfaceBody(fixture, 'Ghost');
  expect(ghost !== null, `tsInterfaceBody must still read the live declaration past ${label}`);
  expect(ghost !== null && /\blive: string/.test(ghost), `${label} must not hide the live declaration`);
  expect(
    ghost !== null && !ghostDecoy.test(ghost),
    `a declaration inside ${label} must not answer for the live one`,
  );
}

const commentedPolicy = tsFunctionBody(commentDecoy, 'readPolicy');
expect(commentedPolicy !== null, 'tsFunctionBody must read the live declaration past a block comment');
expect(
  commentedPolicy !== null && /value\.length < 8/.test(commentedPolicy),
  'a block-commented function must not answer for the live one',
);

// The third vector, and the one `rustStructBody` had too: the closing brace is
// looked for in the code view, so a `\n}` spelled inside a literal cannot end
// the block early and truncate what the check then asserts over.
const bracedPolicy = tsFunctionBody(braceDecoy, 'readPolicy');
expect(
  bracedPolicy !== null && /value\.length < 8/.test(bracedPolicy),
  'a `\\n}` inside a string literal must not truncate the live function body',
);

const COMPONENT = `<script lang="ts">
  // let commentedState = $state(false);
  let liveState = $state(false);

  function validatePassword(value: string) {
    if (value.length < 8) return 'too short';
    // if (!/[A-Z]/.test(value)) return 'needs an uppercase letter';
    if (!/[0-9]/.test(value)) return 'needs a digit';
    return null;
  }
</script>

<!-- <a href="/admin/runners">commented out</a> -->
<a href="/settings/runners">live</a>
<p>See https://example.com/docs for the rest.</p>

<style>
  /* .commented { color: red; } */
  .live { color: green; }
</style>
`;

expect(isSvelteComponent(COMPONENT), 'a Svelte component must be read as one');

const componentSource = productionTsSource(COMPONENT);
expect(componentSource.length === COMPONENT.length, 'the component view must stay byte-aligned');

expect(/href="\/settings\/runners"/.test(componentSource), 'live markup must survive the production view');
expect(
  !/\/admin\/runners/.test(componentSource),
  'markup commented out with `<!-- … -->` must read as deleted — the browser never renders it',
);
expect(/liveState/.test(componentSource), 'live script code must survive the production view');
expect(!/commentedState/.test(componentSource), 'a commented-out `<script>` line must read as deleted');
expect(/\.live \{ color: green; \}/.test(componentSource), 'live CSS must survive the production view');
expect(!/\.commented/.test(componentSource), 'a commented-out CSS rule must read as deleted');

// `//` is a comment opener in a script body and not in prose. Blanking the
// second would be a false red about text the page really does render.
expect(
  /https:\/\/example\.com\/docs/.test(componentSource),
  'a URL in markup text must not be mistaken for a line comment',
);

// The card that put `tsFunctionBody` here: the page carries the whole password
// policy a second time as an HTML `pattern=` attribute, so a file-wide
// `/\[A-Z\]/` went on passing with the rule deleted from the validator
// (card_a08ef8308236). Reading inside the function is what makes the deletion
// visible — and a rule that is merely commented out is deleted.
const validatePassword = tsFunctionBody(COMPONENT, 'validatePassword');
expect(validatePassword !== null, 'tsFunctionBody must read a function nested in a `<script>` block');
expect(
  validatePassword !== null && /value\.length < 8/.test(validatePassword),
  'tsFunctionBody must return the live rules',
);
expect(
  validatePassword !== null && !/\[A-Z\]/.test(validatePassword),
  'a commented-out rule inside a live validator must read as deleted',
);
expect(
  tsFunctionBody(COMPONENT, 'neverDeclared') === null,
  'tsFunctionBody must fail loudly on a function that does not exist',
);

// A component whose markup is its whole body — `web/src/routes/help/+page.svelte`
// is the real one — has no `<script>` tag to detect, and its HTML comments must
// still be blanked.
const MARKUP_ONLY = `<h1>Help</h1>\n<!-- <a href="/ghost">gone</a> -->\n<a href="/real">here</a>\n`;
expect(isSvelteComponent(MARKUP_ONLY), 'a script-less component must still be read as markup');
expect(!/ghost/.test(productionTsSource(MARKUP_ONLY)), 'a script-less component must have its HTML comments blanked');
expect(/\/real/.test(productionTsSource(MARKUP_ONLY)), 'a script-less component must keep its live markup');

if (failures.length > 0) {
  console.error('❌ TypeScript production view contract failed:');
  for (const failure of failures) console.error(`   - ${failure}`);
  process.exit(1);
}

console.log('✅ TypeScript production view contract ok');
