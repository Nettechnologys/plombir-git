#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const routerPath = path.join(root, 'crates/rg-http/src/lib.rs');
const backendPath = path.join(root, 'crates/rg-http/src/api/audit.rs');
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const pagePath = path.join(root, 'web/src/routes/admin/audit/+page.svelte');

const router = readFileSync(routerPath, 'utf8');
const backend = readFileSync(backendPath, 'utf8');
const client = readFileSync(clientPath, 'utf8');
const page = readFileSync(pagePath, 'utf8');
const failures = [];

if (!/\.route\(\s*"\/admin\/audit\/logs",\s*get\(api::audit::list_audit_logs\)/.test(router)) {
  failures.push('Backend router must expose GET /admin/audit/logs');
}

if (!/\.route\(\s*"\/admin\/audit\/logs\/\{id\}",\s*get\(api::audit::get_audit_log\)/.test(router)) {
  failures.push('Backend router must expose GET /admin/audit/logs/{id}');
}

if (!/pub async fn list_audit_logs/.test(backend) || !/pub async fn get_audit_log/.test(backend)) {
  failures.push('Backend audit API must keep list and detail handlers');
}

if (!/page_size:\s*Option<u64>/.test(backend) || !/logs:\s*Vec<AuditLogEntry>/.test(backend)) {
  failures.push('Backend audit list contract must return page_size and logs');
}

if (!/listAuditLogs:\s*\(query\?:\s*AuditLogQuery\)\s*=>[\s\S]*request<AuditLogResponse>\(`\/admin\/audit\/logs/.test(client)) {
  failures.push('API client must call GET /admin/audit/logs for audit listing');
}

if (!/getAuditLog:\s*\(id:\s*number\)\s*=>[\s\S]*request<AuditLogEntry>\(`\/admin\/audit\/logs\/\$\{id\}`\)/.test(client)) {
  failures.push('API client must call GET /admin/audit/logs/{id} for audit detail');
}

if (!/async function openDetail\(log:\s*AuditLogEntry\)[\s\S]*admin\.getAuditLog\(log\.id\)/.test(page)) {
  failures.push('Audit page Details action must fetch the backend detail endpoint');
}

if (!/detailLoading/.test(page)) {
  failures.push('Audit page must expose loading state while fetching detail');
}

if (failures.length > 0) {
  console.error('Audit log frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Audit log frontend/backend contract ok');
