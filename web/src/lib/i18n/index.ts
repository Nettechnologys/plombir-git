import { writable, derived } from 'svelte/store';
import en from './translations/en.json';
import zhCN from './translations/zh-CN.json';
import ru from './translations/ru.json';
import { activeLocale, setActiveLocale, type Locale } from './locale.svelte';
import { parseServerTime } from '../serverTime';

export type { Locale };
export { activeLocale };

type TranslationCatalog = typeof en;

const translations: Record<Locale, TranslationCatalog> = {
  'en': en,
  'zh-CN': zhCN,
  'ru': ru,
};

const dateLocales: Record<Locale, string> = {
  'en': 'en-US',
  'zh-CN': 'zh-CN',
  'ru': 'ru-RU',
};

// Locale detection
function detectLocale(): Locale {
  if (typeof window === 'undefined') return 'en';
  try {
    const stored = localStorage.getItem('locale');
    if (stored && Object.hasOwn(translations, stored)) return stored as Locale;
  } catch {
    // Storage can be blocked by browser policy; language selection still works.
  }
  const language = navigator.language.toLowerCase().split('-')[0];
  if (language === 'zh') return 'zh-CN';
  if (language === 'ru') return 'ru';
  return 'en';
}

// Create locale store
//
// The store stays for `$locale` readers (the switcher), but it is a mirror: the
// value `t()` reads is the `$state` rune in `locale.svelte.ts`, and both are
// written together here so they can never disagree.
function createLocaleStore() {
  const { subscribe, set } = writable<Locale>(activeLocale());

  function apply(locale: Locale) {
    setActiveLocale(locale);
    set(locale);
  }

  return {
    subscribe,
    set: (locale: Locale) => {
      if (typeof window !== 'undefined') {
        try {
          localStorage.setItem('locale', locale);
        } catch {
          // Keep the choice for this page even when it cannot be persisted.
        }
      }
      apply(locale);
    },
    init: () => {
      apply(detectLocale());
    },
  };
}

export const locale = createLocaleStore();

// Current translations
export const currentTranslations = derived(locale, ($locale) => translations[$locale]);

// Translation function
type NestedKeyOf<ObjectType extends object> = {
  [Key in keyof ObjectType & (string | number)]: ObjectType[Key] extends object
    ? `${Key}` | `${Key}.${NestedKeyOf<ObjectType[Key]>}`
    : `${Key}`;
}[keyof ObjectType & (string | number)];

type TranslationKey = NestedKeyOf<typeof en>;

// Deep get
function getNestedValue(obj: any, path: string): string | undefined {
  return path.split('.').reduce((acc, part) => acc?.[part], obj);
}

// Interpolation helper
function interpolate(str: string, params: Record<string, string | number>): string {
  return str.replace(/\{(\w+)\}/g, (_, key) => String(params[key] ?? `{${key}}`));
}

export function formatTranslationFallback(value: unknown): string {
  const readable = typeof value === 'string'
    ? value.replace(/[._-]+/g, ' ').trim()
    : '';
  return readable ? readable[0].toUpperCase() + readable.slice(1) : 'Unknown';
}

type TranslationParams = Record<string, string | number>;
type TranslationOptions = TranslationParams | string;
type Translator = {
  (key: string, params?: TranslationParams): string;
  (key: string, fallback?: string): string;
  (key: string, params: TranslationParams | undefined, fallback: string): string;
};

// Reads the `$state` locale, so any template, `$derived` or `$effect` that
// calls `t()` re-runs when the language is switched.
function activeCatalog(): TranslationCatalog {
  return translations[activeLocale()];
}

function resolveTranslation(
  key: string,
  options?: TranslationOptions,
  catalog: TranslationCatalog = activeCatalog(),
  dynamicFallback?: string,
): string {
  const value = getNestedValue(catalog, key);
  const fallback = typeof options === 'string' ? options : dynamicFallback ?? key;
  if (typeof value !== 'string') {
    console.warn(`[i18n] Missing translation: "${key}"`);
    return fallback;
  }
  if (options && typeof options !== 'string') {
    return interpolate(value, options);
  }
  return value;
}

// Main t() function
export function t(key: string, params?: TranslationParams): string;
export function t(key: string, fallback?: string): string;
export function t(key: string, params: TranslationParams | undefined, fallback: string): string;
export function t(key: string, options?: TranslationOptions, dynamicFallback?: string): string {
  return resolveTranslation(key, options, undefined, dynamicFallback);
}

// Reactive t() for Svelte components
// Returns a plain function (not a store) for easy usage in both script and
// template. It looks the catalog up on every call, so a template expression
// that calls it follows a language switch; a value computed once at the top of
// a `<script>` does not — wrap such values in `$derived` or a function.
export function createT() {
  const translate: Translator = (
    key: string,
    options?: TranslationOptions,
    dynamicFallback?: string,
  ): string => resolveTranslation(key, options, activeCatalog(), dynamicFallback);
  return translate;
}

// Date formatting with locale
export function formatDate(dateStr: string, options?: Intl.DateTimeFormatOptions): string {
  const $locale = activeLocale();
  const date = parseServerTime(dateStr);
  const defaultOptions: Intl.DateTimeFormatOptions = {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  };
  return date.toLocaleDateString(dateLocales[$locale], options ?? defaultOptions);
}

/**
 * The form of a counted noun the active locale uses for `count`: "1 branch",
 * "2 branches"; "1 ветка", "3 ветки", "5 веток". The forms are passed in, each
 * from its own literal key, so the catalog check still sees every key.
 */
export function pluralForm(
  count: number,
  forms: { one: string; few: string; many: string; other: string },
): string {
  const category = new Intl.PluralRules(dateLocales[activeLocale()]).select(count);
  return category === 'one' || category === 'few' || category === 'many' ? forms[category] : forms.other;
}

/**
 * How long ago `dateStr` was, in the active locale ("3 days ago", "3 дня
 * назад"), the way GitHub labels commits. Past a year it is the date itself.
 */
export function formatRelativeTime(dateStr: string, now: number = Date.now()): string {
  const $locale = activeLocale();
  const seconds = Math.round((parseServerTime(dateStr).getTime() - now) / 1000);
  const steps: [Intl.RelativeTimeFormatUnit, number][] = [
    ['second', 60],
    ['minute', 60],
    ['hour', 24],
    ['day', 30],
    ['month', 12],
  ];
  let value = seconds;
  for (const [unit, size] of steps) {
    if (Math.abs(value) < size) {
      return new Intl.RelativeTimeFormat(dateLocales[$locale], { numeric: 'auto' }).format(value, unit);
    }
    value = Math.round(value / size);
  }
  return formatDate(dateStr);
}

export function formatDateTime(dateStr: string): string {
  const $locale = activeLocale();
  const date = parseServerTime(dateStr);
  return date.toLocaleString(dateLocales[$locale], {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}
