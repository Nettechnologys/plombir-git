import { request, type PaginatedResponse, type PaginationMeta } from './_base.svelte';

interface RunnerAdminResponse {
  id: number;
  name: string;
  status: string;
  labels: string | string[];
  last_seen_at: string;
  version: string | null;
  os: string | null;
  arch: string | null;
}

interface RunnerListItem {
  id: number;
  name: string;
  status: string;
  labels: string[];
  last_seen: string;
  last_seen_at: string;
  version: string | null;
  os: string | null;
  arch: string | null;
}

export interface RegisterRunnerResponse {
  id: number;
  token: string;
  message: string;
}

function toPagination(total: number, page?: number, perPage?: number): PaginationMeta {
  const safePage = Math.max(1, Number(page ?? 1));
  const safePerPage = Number(perPage ?? 20);
  const effectivePerPage = safePerPage > 0 ? safePerPage : 20;
  const totalPages = total === 0 ? 1 : Math.max(1, Math.ceil(total / effectivePerPage));
  return {
    page: safePage,
    per_page: effectivePerPage,
    total,
    total_pages: totalPages,
    has_next: safePage < totalPages,
    has_prev: safePage > 1,
  };
}

function parseRunnerLabels(labels: string | string[] | undefined | null): string[] {
  if (Array.isArray(labels)) {
    return labels;
  }

  if (!labels || typeof labels !== 'string') {
    return [];
  }

  try {
    const parsed = JSON.parse(labels);
    if (Array.isArray(parsed)) {
      return parsed
        .filter((v) => typeof v === 'string')
        .map((v) => v as string)
        .map((v) => v.trim())
        .filter(Boolean);
    }
  } catch {
    // Keep backward compatibility with older plain string storage.
  }

  return labels
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
}

function normalizeRunner(row: RunnerAdminResponse): RunnerListItem {
  const parsedLabels = parseRunnerLabels(row.labels);
  return {
    id: row.id,
    name: row.name,
    status: row.status,
    labels: parsedLabels,
    last_seen: row.last_seen_at,
    last_seen_at: row.last_seen_at,
    version: row.version,
    os: row.os,
    arch: row.arch,
  };
}

export const runners = {
  list: (page?: number, perPage?: number) =>
    request<RunnerAdminResponse[]>(`/admin/runners`).then((rows) => {
      const list = rows.map(normalizeRunner);
      const start = ((page ?? 1) - 1) * (perPage ?? 20);
      const size = perPage ?? 20;
      return {
        data: list.slice(start, start + size),
        pagination: toPagination(list.length, page, perPage),
      } as PaginatedResponse<RunnerListItem>;
    }),
  get: (id: number) =>
    request<RunnerAdminResponse>(`/admin/runners/${id}`).then(normalizeRunner),
  register: (data: { name: string; labels?: string[] }) =>
    request<RegisterRunnerResponse>('/runners/register', {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  delete: (id: number) =>
    request<{ deleted: boolean }>(`/admin/runners/${id}`, { method: 'DELETE' }),
};
