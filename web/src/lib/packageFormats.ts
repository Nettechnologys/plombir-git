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

export function packageFormatSupportLabel(format: string): string {
  return packageFormatUsesGenericFallback(format) ? 'Generic fallback' : 'Native adapter';
}

export function packageFormatOptionLabel(format: string): string {
  const label = packageFormatLabel(format);
  return packageFormatUsesGenericFallback(format) ? `${label} (Generic fallback)` : label;
}
