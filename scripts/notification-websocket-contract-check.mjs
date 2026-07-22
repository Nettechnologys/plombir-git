#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const client = readFileSync(clientPath, 'utf8');

const failures = [];

function expect(pattern, message) {
  if (!pattern.test(client)) failures.push(message);
}

function reject(pattern, message) {
  if (pattern.test(client)) failures.push(message);
}

expect(
  /function\s+withWebSocketApiBase\s*\([^)]*\)[\s\S]*new\s+URL\s*\(\s*API_BASE\s*,\s*window\.location\.origin\s*\)/,
  'Notification WebSocket URL must be based on configured API_BASE',
);
expect(
  /apiUrl\.protocol\s*===\s*['"]https:['"][\s\S]*['"]wss:['"]/,
  'Notification WebSocket URL must translate HTTPS API bases to WSS',
);
expect(
  /withWebSocketApiBase\s*\(\s*['"]\/ws\/notifications['"]\s*\)[\s\S]*encodeURIComponent\s*\(\s*token\s*\)/,
  'Notification WebSocket connection must append the encoded token to the API-based URL',
);
reject(
  /const\s+wsUrl\s*=\s*`\$\{protocol\}\/\/\$\{window\.location\.host\}/,
  'Notification WebSocket must not use the frontend host directly',
);

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Notification WebSocket frontend/backend contract ok');
