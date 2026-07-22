import { withApiBase, request, qs, type PaginationMeta } from './_base.svelte';

interface PackageSummaryResponse {
  id: number;
  name: string;
  description: string | null;
  homepage: string | null;
  version_count: number;
  latest_version: string | null;
  download_count: number;
  keywords: string | null;
  format?: string;
}

interface PackageRegistry {
  package_type: string;
  enabled: boolean;
}

interface PackageListByTypeResponse {
  packages: PackageSummaryResponse[];
}

interface PackageVersionResponse {
  id: number;
  version: string;
  semver: string | null;
  metadata: string | null;
  size: number;
  sha256: string | null;
  is_yanked: boolean;
  download_count: number;
  files: PackageFileResponse[];
  created_at: string;
}

interface PackageFileResponse {
  id: number;
  filename: string;
  size: number;
  sha256: string | null;
}

interface VersionListByTypeResponse {
  versions: PackageVersionResponse[];
}

interface PublishResponse {
  package_id: number;
  version_id: number;
  existing: boolean;
}

interface RegistryListResponse {
  registries: PackageRegistry[];
}

function encodeRepoPath(path: string): string {
  return path.split('/').map(encodeURIComponent).join('/');
}

function contentDispositionAttachment(filename: string): string {
  return `attachment; filename*=UTF-8''${encodeURIComponent(filename || 'file')}`;
}

function toPagination(total: number, page?: number, perPage?: number): PaginationMeta {
  const currentPage = page ?? 1;
  const size = perPage ?? 20;
  const totalPages = Math.max(1, Math.ceil(total / size));
  return {
    page: currentPage,
    per_page: size,
    total,
    total_pages: totalPages,
    has_next: currentPage < totalPages,
    has_prev: currentPage > 1,
  };
}

function filterPackagesByQuery(packages: PackageSummaryResponse[], query?: string): PackageSummaryResponse[] {
  const needle = (query || '').trim().toLowerCase();
  if (!needle) return packages;

  return packages.filter((pkg) => {
    const haystack = [
      pkg.name,
      pkg.description,
      pkg.latest_version,
      pkg.keywords,
      pkg.format,
    ]
      .filter(Boolean)
      .join(' ')
      .toLowerCase();

    return haystack.includes(needle);
  });
}

export const packages = {
  list: async (owner: string, repo: string, pkg_type?: string, page?: number, perPage?: number, query?: string) => {
    if (pkg_type) {
      const res = await request<PackageListByTypeResponse>(`/repos/${owner}/${repo}/packages/${encodeURIComponent(pkg_type)}/list`);
      const list = filterPackagesByQuery(
        (res.packages || []).map((item) => ({ ...item, format: pkg_type })),
        query,
      );
      const start = ((page ?? 1) - 1) * (perPage ?? 20);
      const size = perPage ?? 20;
      return {
        data: list.slice(start, start + size),
        pagination: toPagination(list.length, page, perPage),
      };
    }

    const reg = await request<RegistryListResponse>(`/repos/${owner}/${repo}/packages`);
    const regTypes = reg.registries.filter((r) => r.enabled).map((r) => r.package_type);

    if (regTypes.length === 0) {
      return {
        data: [] as PackageSummaryResponse[],
        pagination: toPagination(0, page, perPage),
      };
    }

    const packByType = await Promise.all(
      regTypes.map((pkg_type) => request<PackageListByTypeResponse>(`/repos/${owner}/${repo}/packages/${encodeURIComponent(pkg_type)}/list`).catch(() => ({ packages: [] })))
    );
    const list = packByType.flatMap((group, idx) =>
      (group.packages || []).map((pkg) => ({ ...pkg, format: regTypes[idx] }))
    );
    const filteredList = filterPackagesByQuery(list, query);
    const start = ((page ?? 1) - 1) * (perPage ?? 20);
    const size = perPage ?? 20;

    return {
      data: filteredList.slice(start, start + size),
      pagination: toPagination(filteredList.length, page, perPage),
    };
  },
  getFormat: (owner: string, repo: string, pkg_type: string) =>
    request<PackageListByTypeResponse>(`/repos/${owner}/${repo}/packages/${encodeURIComponent(pkg_type)}/list`),
  get: (owner: string, repo: string, pkg_type: string, pkg_name: string) =>
    request<any>(`/repos/${owner}/${repo}/packages/${encodeURIComponent(pkg_type)}/${encodeURIComponent(pkg_name)}`),
  getVersions: (owner: string, repo: string, pkg_type: string, pkg_name: string) =>
    request<VersionListByTypeResponse>(`/repos/${owner}/${repo}/packages/${encodeURIComponent(pkg_type)}/${encodeURIComponent(pkg_name)}/versions`),
  getVersion: (owner: string, repo: string, pkg_type: string, pkg_name: string, version: string) =>
    request<any>(`/repos/${owner}/${repo}/packages/${encodeURIComponent(pkg_type)}/${encodeURIComponent(pkg_name)}/${encodeURIComponent(version)}`),
  downloadUrl: (owner: string, repo: string, pkg_type: string, pkg_name: string, version: string, filename: string) =>
    withApiBase(`/repos/${owner}/${repo}/packages/${encodeURIComponent(pkg_type)}/${encodeURIComponent(pkg_name)}/${encodeURIComponent(version)}/${encodeRepoPath(filename)}`),
  publish: (owner: string, repo: string, pkg_type: string, body: Blob | string, metadata?: { name?: string; version?: string; description?: string; homepage?: string; repository_url?: string; semver?: string }) => {
    const query = qs({
      name: metadata?.name,
      version: metadata?.version,
      description: metadata?.description,
      homepage: metadata?.homepage,
      repository_url: metadata?.repository_url,
      semver: metadata?.semver,
    });
    const headers: Record<string, string> = {
      'Content-Type': 'application/octet-stream',
    };
    if (body instanceof Blob && 'name' in body) {
      const filename = (body as File).name || 'package';
      headers['Content-Disposition'] = contentDispositionAttachment(filename);
    }
    return request<PublishResponse>(`/repos/${owner}/${repo}/packages/${encodeURIComponent(pkg_type)}/publish${query}`, {
      method: 'POST',
      body: body instanceof Blob ? body : (body as string),
      headers,
    } as RequestInit);
  },
  create: (owner: string, repo: string, pkg_type: string, data: { name: string; version: string; description?: string; homepage?: string; repository_url?: string; semver?: string; file?: File }) => {
    if (!data.file) {
      throw new Error('Package file is required');
    }
    return packages.publish(owner, repo, pkg_type, data.file, {
      name: data.name,
      version: data.version,
      description: data.description,
      homepage: data.homepage,
      repository_url: data.repository_url,
      semver: data.semver,
    });
  },
  delete: (owner: string, repo: string, pkg_type: string, pkg_name: string, version: string) =>
    request<void>(`/repos/${owner}/${repo}/packages/${encodeURIComponent(pkg_type)}/${encodeURIComponent(pkg_name)}/${encodeURIComponent(version)}`, { method: 'DELETE' }),
};
