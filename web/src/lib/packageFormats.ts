import { t } from '$lib/i18n';

export const PACKAGE_FORMATS = [
  'cargo',
  'npm',
  'maven',
  'pypi',
  'docker',
  'nuget',
  'rubygems',
  'go',
  'helm',
  'composer',
  'conan',
  'conda',
  'alpine',
  'debian',
  'rpm',
  'swift',
  'generic',
] as const;

export type PackageFormat = (typeof PACKAGE_FORMATS)[number];

export const NATIVE_PACKAGE_FORMATS = [
  'cargo',
  'npm',
  'maven',
  'pypi',
  'docker',
  'nuget',
  'rubygems',
  'helm',
  'composer',
  'generic',
] as const;

const NATIVE_FORMAT_SET = new Set<string>(NATIVE_PACKAGE_FORMATS);

export const PACKAGE_FORMAT_LABELS: Record<string, string> = {
  cargo: 'Cargo',
  npm: 'npm',
  maven: 'Maven',
  pypi: 'PyPI',
  docker: 'Docker',
  nuget: 'NuGet',
  rubygems: 'RubyGems',
  go: 'Go',
  helm: 'Helm',
  composer: 'Composer',
  conan: 'Conan',
  conda: 'Conda',
  alpine: 'Alpine',
  debian: 'Debian',
  rpm: 'RPM',
  swift: 'Swift',
  generic: 'Generic',
};

export function packageFormatLabel(format: string): string {
  return PACKAGE_FORMAT_LABELS[format] || format;
}

export function packageFormatUsesGenericFallback(format: string): boolean {
  return !NATIVE_FORMAT_SET.has(format);
}

// Translated on every call: a template that calls these follows a language
// switch, because `t()` reads the reactive locale (card_0d18cf31d13b).
export function packageFormatSupportLabel(format: string): string {
  return packageFormatUsesGenericFallback(format)
    ? t('packages.support.generic_fallback')
    : t('packages.support.native_adapter');
}

export function packageFormatOptionLabel(format: string): string {
  const label = packageFormatLabel(format);
  return packageFormatUsesGenericFallback(format)
    ? t('packages.support.option_generic_fallback', { label })
    : label;
}
