// The one piece of i18n state Svelte can see.
//
// `t()` used to look its catalog up through `get(currentTranslations)`. `get()`
// reads a store once and registers nothing, so a template expression
// `{t('…')}` never learned that it depended on the locale: switching language
// changed `localStorage` and the switcher's own label (which reads `$locale`)
// and left every other string on the open page in the old language until the
// next navigation (card_0d18cf31d13b).
//
// A `$state` rune read inside `t()` makes every caller — a template expression,
// a `$derived`, an `$effect` — depend on the locale without knowing it does.

export type Locale = 'en' | 'zh-CN';

let current = $state<Locale>('en');

/** The active locale. Reading it inside an effect or template subscribes to it. */
export function activeLocale(): Locale {
	return current;
}

/**
 * Switch the active locale, and say so to the document: `<html lang>` drives
 * screen-reader pronunciation, hyphenation and font fallback for CJK text, so
 * it has to follow the switch rather than stay at the `lang="en"` that
 * `app.html` ships with.
 */
export function setActiveLocale(locale: Locale): void {
	current = locale;
	if (typeof document !== 'undefined') {
		document.documentElement.lang = locale;
	}
}
