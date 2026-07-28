import assert from 'node:assert/strict';
import { createServer } from 'vite';

// `noDiscovery` turns off the dependency pre-bundling scan. We only ever
// `ssrLoadModule` one leaf module, so the scan is pure overhead — and because it
// runs in the background it loses a race with `server.close()` below and prints
// ~100 lines of "Failed to scan for dependencies" / "server is being restarted
// or closed" onto a run that still exits 0. A CI gate whose green output is full
// of stack traces is a gate people stop reading.
const server = await createServer({
  logLevel: 'error',
  server: { middlewareMode: true },
  appType: 'custom',
  optimizeDeps: { noDiscovery: true },
});

try {
  const { renderMarkdown, sanitizeHtml } = await server.ssrLoadModule('/src/lib/utils/markdown.ts');

  const link = renderMarkdown('[x](javascript:alert(1))');
  assert.equal(link.includes('javascript:'), false);
  assert.equal(link.includes('href='), false);

  const image = sanitizeHtml('<p><img src="/logo.png" onerror="alert(1)"></p>');
  assert.equal(image.includes('onerror'), false);
  assert.equal(image.includes('src="/logo.png"'), true);

  const svg = sanitizeHtml('<svg><script>alert(1)</script></svg><strong>ok</strong>');
  assert.equal(svg.includes('<svg'), false);
  assert.equal(svg.includes('<script'), false);
  assert.equal(svg.includes('<strong>ok</strong>'), true);

  const html = sanitizeHtml('<a href="https://example.com" onclick="alert(1)">safe</a>');
  assert.equal(html.includes('onclick'), false);
  assert.equal(html.includes('href="https://example.com"'), true);

  console.log('markdown sanitizer smoke: 4 checks passed');
} finally {
  await server.close();
}
