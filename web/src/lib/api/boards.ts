import { request } from './_base.svelte';

import type { Issue } from './issues';

export interface Board {
  id: number;
  repo_id: number | null;
  org_id: number | null;
  name: string;
  description: string | null;
  created_by: number | null;
  created_at: string;
  updated_at: string;
}

export interface BoardCard {
  id: number;
  column_id: number;
  issue_id: number | null;
  note: string | null;
  position: number;
  created_at: string;
  updated_at: string;
  issue?: Issue | null;
}

export interface BoardColumn {
  id: number;
  board_id: number;
  name: string;
  color: string | null;
  position: number;
  created_at: string;
  cards: BoardCard[];
}

interface BoardColumnResponse {
  column: Omit<BoardColumn, 'cards'>;
  cards: BoardCard[];
}

export interface BoardFullResponse {
  board: Board;
  columns: BoardColumnResponse[];
}

export interface BoardUpdatePayload {
  name?: string;
  description?: string;
}

export interface BoardCardUpdatePayload {
  note?: string;
  issue_id?: number | null;
}

export const boards = {
  list: (owner: string, repo: string) =>
    request<Board[]>(`/repos/${owner}/${repo}/boards`),
  get: (owner: string, repo: string, id: number) =>
    request<BoardFullResponse>(`/repos/${owner}/${repo}/boards/${id}`),
  create: (owner: string, repo: string, data: { name: string; description?: string }) =>
    request<Board>(`/repos/${owner}/${repo}/boards`, { method: 'POST', body: JSON.stringify(data) }),
  update: (owner: string, repo: string, id: number, data: BoardUpdatePayload) =>
    request<Board>(`/repos/${owner}/${repo}/boards/${id}`, { method: 'PATCH', body: JSON.stringify(data) }),
  delete: (owner: string, repo: string, id: number) =>
    request<void>(`/repos/${owner}/${repo}/boards/${id}`, { method: 'DELETE' }),
  createColumn: (owner: string, repo: string, boardId: number, data: { name: string }) =>
    request<Omit<BoardColumn, 'cards'>>(`/repos/${owner}/${repo}/boards/${boardId}/columns`, { method: 'POST', body: JSON.stringify(data) }),
  updateColumn: (owner: string, repo: string, boardId: number, colId: number, data: { name?: string }) =>
    request<Omit<BoardColumn, 'cards'>>(`/repos/${owner}/${repo}/boards/${boardId}/columns/${colId}`, { method: 'PATCH', body: JSON.stringify(data) }),
  deleteColumn: (owner: string, repo: string, boardId: number, colId: number) =>
    request<void>(`/repos/${owner}/${repo}/boards/${boardId}/columns/${colId}`, { method: 'DELETE' }),
  createCard: (owner: string, repo: string, boardId: number, colId: number, data: { note?: string; issue_id?: number }) =>
    request<BoardCard>(`/repos/${owner}/${repo}/boards/${boardId}/columns/${colId}/cards`, { method: 'POST', body: JSON.stringify(data) }),
  updateCard: (owner: string, repo: string, boardId: number, cardId: number, data: BoardCardUpdatePayload) =>
    request<BoardCard>(`/repos/${owner}/${repo}/boards/${boardId}/cards/${cardId}`, { method: 'PATCH', body: JSON.stringify(data) }),
  moveCard: (owner: string, repo: string, boardId: number, cardId: number, data: { column_id: number; position: number }) =>
    request<BoardCard>(`/repos/${owner}/${repo}/boards/${boardId}/cards/${cardId}/move`, { method: 'POST', body: JSON.stringify(data) }),
  reorderCards: (owner: string, repo: string, boardId: number, data: { column_id: number; positions: [number, number][] }) =>
    request<{ status: string }>(`/repos/${owner}/${repo}/boards/${boardId}/cards/reorder`, { method: 'POST', body: JSON.stringify(data) }),
  deleteCard: (owner: string, repo: string, boardId: number, cardId: number) =>
    request<void>(`/repos/${owner}/${repo}/boards/${boardId}/cards/${cardId}`, { method: 'DELETE' }),
};
