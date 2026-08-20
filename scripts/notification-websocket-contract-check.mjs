#!/usr/bin/env node

// Contract: the notification WebSocket client must build its URL from the
// configured API_BASE (never the frontend host) and authenticate via the
// HttpOnly `forgekeep_token` cookie the browser sends automatically on a
// same-origin upgrade (M-4/M-5). The token must NOT be leaked into the URL
// query string, nor passed as a Sec-WebSocket-Protocol subprotocol — the
// backend `ws_notifications_handler` reads the cookie first, through
// crates/rg-http/src/api/auth.rs `ws_session` — which is also where the cookie's
// name lives (`AUTH_COOKIE_NAME`). Renaming it means updating this file too.

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { productionTsSource } from './lib/ts-source.mjs';

const root = process.cwd();
const wsPath = path.join(root, 'web/src/lib/api/websockets.ts');
const source = productionTsSource(readFileSync(wsPath, 'utf8'));
const notificationsPage = productionTsSource(readFileSync(
  path.join(root, 'web/src/routes/notifications/+page.svelte'),
  'utf8',
));

const failures = [];

function expect(pattern, message) {
  if (!pattern.test(source)) failures.push(message);
}

function reject(pattern, message) {
  if (pattern.test(source)) failures.push(message);
}

// URL is derived from the configured API_BASE, resolved against the origin.
expect(
  /new\s+URL\s*\(\s*API_BASE\s*,\s*window\.location\.origin\s*\)/,
  'Notification WebSocket URL must be based on the configured API_BASE',
);

// HTTPS API bases upgrade to WSS, everything else to WS.
expect(
  /apiUrl\.protocol\s*===\s*['"]https:['"]\s*\?\s*['"]wss:['"]\s*:\s*['"]ws:['"]/,
  'Notification WebSocket URL must translate HTTPS API bases to WSS',
);

// The notification socket connects to the API-based /ws/notifications path.
expect(
  /connectNotificationWebSocket[\s\S]*?new\s+WebSocket\s*\(\s*withWebSocketApiBase\s*\(\s*['"]\/ws\/notifications['"]\s*\)\s*\)/,
  'Notification WebSocket must connect to the API-based /ws/notifications URL',
);

// M-4/M-5 cookie auth: the token must never be appended to the URL.
reject(
  /encodeURIComponent\s*\(\s*token\s*\)/,
  'Notification WebSocket must not append the token to the URL — auth is the HttpOnly forgekeep_token cookie',
);
reject(
  /[?&]token=/,
  'Notification WebSocket must not carry the token in the query string (leaks into logs / history)',
);
reject(
  /connectNotificationWebSocket\s*\([^)]*\btoken\b/,
  'connectNotificationWebSocket must not take a token argument — the browser sends the auth cookie',
);

// Cookie auth also means no Sec-WebSocket-Protocol subprotocol argument: the
// second `new WebSocket(url, protocols)` argument would signal token-in-protocol.
reject(
  /new\s+WebSocket\s*\(\s*withWebSocketApiBase\s*\(\s*['"]\/ws\/notifications['"]\s*\)\s*,/,
  'Notification WebSocket must not pass a Sec-WebSocket-Protocol argument — auth is the cookie',
);

// The URL host must come from the API base, not the frontend host.
reject(
  /new\s+WebSocket\s*\([^)]*window\.location\.host/,
  'Notification WebSocket must not use the frontend host directly',
);

// A rejected handshake fires `error` + `close`, but never `open`; browsers do
// not expose its HTTP status. Do not turn that observable shape into a retry
// loop, and re-check the live auth store before retrying a previously-open socket.
expect(
  /ws\.onopen\s*=\s*\(\)\s*=>\s*\{[\s\S]*?opened\s*=\s*true/,
  'Notification WebSocket must remember whether the handshake reached open',
);
expect(
  /ws\.onclose\s*=\s*\(\)\s*=>\s*\{[\s\S]*?if\s*\(\s*!opened\s*\)\s*return[\s\S]*?if\s*\(\s*!shouldReconnect\s*\(\s*\)\s*\)\s*return/,
  'Notification WebSocket must not retry a failed handshake and must re-check auth before reconnecting',
);
expect(
  /connectNotificationWebSocket\s*\(\s*onMessage\s*,\s*onError\s*,\s*shouldReconnect\s*\)/,
  'Notification WebSocket recursion must preserve the auth predicate',
);
if (
  !/connectNotificationWebSocket\s*\([\s\S]*?isLoggedIn\s*,\s*\)/.test(notificationsPage)
) {
  failures.push('Notifications page must gate reconnects on the live auth store');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Notification WebSocket frontend/backend contract ok');
