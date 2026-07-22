import { request, qs } from './_base.svelte';

export interface SearchResult {
  result_type: string;
  id: number;
  title: string;
  excerpt: string | null;
  repo_owner: string | null;
  repo_name: string | null;
  state?: string | null;
  number?: number | null;
}

export interface SearchResponse {
  results: SearchResult[];
  total: number;
  page: number;
  per_page: number;
}

export const search = {
  search: (q: string, type?: string, page?: number, perPage?: number) =>
    request<SearchResponse>(`/search${qs({ q, type: type || 'all', page, per_page: perPage })}`),
};
