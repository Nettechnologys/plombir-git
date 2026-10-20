import { request } from './_base.svelte';
import { pathSegment, repoPath } from './repoPath';

/** A `vars.*` configuration variable: shown back, unlike a secret. */
export interface CiVariable { name: string; value: string; created_at: string; updated_at: string; }

/** Whose variables: a repository's, or an organization's every repository reads. */
export type CiVariableScope = { kind: 'repo'; owner: string; repo: string } | { kind: 'org'; name: string };

const repoVariables = {
  list: (owner: string, repo: string) => request<CiVariable[]>(`${repoPath(owner, repo)}/actions/variables`),
  put: (owner: string, repo: string, variable_name: string, value: string) => request<void>(`${repoPath(owner, repo)}/actions/variables/${encodeURIComponent(variable_name)}`, { method: 'PUT', body: JSON.stringify({ value }) }),
  delete: (owner: string, repo: string, variable_name: string) => request<void>(`${repoPath(owner, repo)}/actions/variables/${encodeURIComponent(variable_name)}`, { method: 'DELETE' }),
};

const orgVariables = {
  list: (name: string) => request<CiVariable[]>(`/orgs/${pathSegment(name)}/actions/variables`),
  put: (name: string, variable_name: string, value: string) => request<void>(`/orgs/${pathSegment(name)}/actions/variables/${encodeURIComponent(variable_name)}`, { method: 'PUT', body: JSON.stringify({ value }) }),
  delete: (name: string, variable_name: string) => request<void>(`/orgs/${pathSegment(name)}/actions/variables/${encodeURIComponent(variable_name)}`, { method: 'DELETE' }),
};

export const ciVariables = {
  list: (scope: CiVariableScope) =>
    scope.kind === 'repo' ? repoVariables.list(scope.owner, scope.repo) : orgVariables.list(scope.name),
  put: (scope: CiVariableScope, name: string, value: string) =>
    scope.kind === 'repo' ? repoVariables.put(scope.owner, scope.repo, name, value) : orgVariables.put(scope.name, name, value),
  delete: (scope: CiVariableScope, name: string) =>
    scope.kind === 'repo' ? repoVariables.delete(scope.owner, scope.repo, name) : orgVariables.delete(scope.name, name),
};
