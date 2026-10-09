import { writable, derived } from 'svelte/store';
import en from './translations/en.json';
import zhCN from './translations/zh-CN.json';
import { activeLocale, setActiveLocale, type Locale } from './locale.svelte';

export type { Locale };
export { activeLocale };

type TranslationCatalog = typeof en;

const translations: Record<Locale, TranslationCatalog> = {
  'en': en,
  'zh-CN': zhCN,
};

// Locale detection
function detectLocale(): Locale {
  if (typeof window === 'undefined') return 'en';
  const stored = localStorage.getItem('locale') as Locale;
  if (stored && translations[stored]) return stored;
  const browser = navigator.language;
  if (browser.startsWith('zh')) return 'zh-CN';
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
        localStorage.setItem('locale', locale);
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
  const date = new Date(dateStr);
  const defaultOptions: Intl.DateTimeFormatOptions = {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  };
  return date.toLocaleDateString($locale === 'zh-CN' ? 'zh-CN' : 'en-US', options ?? defaultOptions);
}

export function formatDateTime(dateStr: string): string {
  const $locale = activeLocale();
  const date = new Date(dateStr);
  return date.toLocaleString($locale === 'zh-CN' ? 'zh-CN' : 'en-US', {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}
