import { writable, derived, get } from 'svelte/store';
import en from './translations/en.json';
import zhCN from './translations/zh-CN.json';

export type Locale = 'en' | 'zh-CN';

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
function createLocaleStore() {
  const { subscribe, set, update } = writable<Locale>('en');

  return {
    subscribe,
    set: (locale: Locale) => {
      if (typeof window !== 'undefined') {
        localStorage.setItem('locale', locale);
      }
      set(locale);
    },
    init: () => {
      const detected = detectLocale();
      set(detected);
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

type TranslationParams = Record<string, string | number>;
type TranslationOptions = TranslationParams | string;
type Translator = {
  (key: string, params?: TranslationParams): string;
  (key: string, fallback?: string): string;
};

function resolveTranslation(
  key: string,
  options?: TranslationOptions,
  catalog: TranslationCatalog = get(currentTranslations),
): string {
  const value = getNestedValue(catalog, key);
  const fallback = typeof options === 'string' ? options : key;
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
export function t(key: string, options?: TranslationOptions): string {
  return resolveTranslation(key, options);
}

// Reactive t() for Svelte components
// Returns a plain function (not a store) for easy usage in both script and template
export function createT() {
  const translate: Translator = (key: string, options?: TranslationOptions): string => {
    const translations = get(currentTranslations);
    return resolveTranslation(key, options, translations);
  };
  return translate;
}

// Date formatting with locale
export function formatDate(dateStr: string, options?: Intl.DateTimeFormatOptions): string {
  const $locale = get(locale);
  const date = new Date(dateStr);
  const defaultOptions: Intl.DateTimeFormatOptions = {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  };
  return date.toLocaleDateString($locale === 'zh-CN' ? 'zh-CN' : 'en-US', options ?? defaultOptions);
}

export function formatDateTime(dateStr: string): string {
  const $locale = get(locale);
  const date = new Date(dateStr);
  return date.toLocaleString($locale === 'zh-CN' ? 'zh-CN' : 'en-US', {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}
