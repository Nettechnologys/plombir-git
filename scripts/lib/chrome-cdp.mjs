import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const POLL_INTERVAL_MS = 25;

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function requestedCdpPort(value) {
  if (value === undefined || value === null || String(value).trim() === '') return 0;

  const port = Number(value);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`CDP_PORT must be an integer from 1 to 65535, got ${JSON.stringify(value)}`);
  }
  return port;
}

function watchChild(child, chromePath) {
  const state = { ended: false, failure: null };
  child.once('error', (error) => {
    state.ended = true;
    state.failure = new Error(`could not start Chrome at ${chromePath}: ${error.message}`);
  });
  child.once('exit', (code, signal) => {
    state.ended = true;
    if (code !== 0 || signal) {
      const ending = signal ? `signal ${signal}` : `exit ${code}`;
      state.failure = new Error(`Chrome ended before its debugger was ready (${ending})`);
    }
  });
  return state;
}

function throwIfChildEnded(state) {
  if (state.failure) throw state.failure;
  if (state.ended) throw new Error('Chrome ended before its debugger was ready');
}

async function publishedCdpPort(profileDir, state, deadline) {
  const activePortFile = join(profileDir, 'DevToolsActivePort');
  let lastMalformed = null;

  while (Date.now() < deadline) {
    throwIfChildEnded(state);
    try {
      const [portLine, browserPath] = readFileSync(activePortFile, 'utf8').trim().split(/\r?\n/);
      const port = Number(portLine);
      if (Number.isInteger(port) && port > 0 && port <= 65535 &&
          browserPath?.startsWith('/devtools/browser/')) {
        return port;
      }
      lastMalformed = `malformed ${activePortFile}`;
    } catch (error) {
      if (error?.code !== 'ENOENT') throw error;
    }
    await sleep(POLL_INTERVAL_MS);
  }

  throw new Error(lastMalformed || `Chrome did not publish ${activePortFile}`);
}

async function waitForDebugger(cdpRoot, state, deadline) {
  let lastError = null;
  while (Date.now() < deadline) {
    throwIfChildEnded(state);
    try {
      const remainingMs = Math.max(1, deadline - Date.now());
      const response = await fetch(`${cdpRoot}/json/version`, {
        signal: AbortSignal.timeout(Math.min(250, remainingMs)),
      });
      if (response.ok) return;
      lastError = `HTTP ${response.status}`;
    } catch (error) {
      lastError = error.message;
    }
    await sleep(POLL_INTERVAL_MS);
  }

  const detail = lastError ? `: ${lastError}` : '';
  throw new Error(`Chrome debugger did not become available at ${cdpRoot}${detail}`);
}

async function stopChild(child, state) {
  if (!child || state?.ended) return;
  // Signal the whole process group, not the browser process. Chrome forks a
  // zygote, a GPU process and one renderer per tab, and every one of them keeps
  // writing into the profile directory; killing the parent alone leaves them
  // running long enough for the `rmSync` below to walk a directory that is
  // still growing, which surfaced as `ENOTEMPTY … rmdir '<profile>/Default'`
  // and a red smoke run over a browser check that had already passed. The
  // launch asks for `detached: true` so the group exists to be signalled; a
  // platform that refuses the negative pid still gets the single-process kill.
  try {
    process.kill(-child.pid, 'SIGKILL');
  } catch {
    try {
      child.kill('SIGKILL');
    } catch {
      // A concurrently exiting child no longer needs a signal; still await/clean below.
    }
  }

  await new Promise((resolve) => {
    let timer;
    const done = () => {
      clearTimeout(timer);
      child.removeListener('exit', done);
      child.removeListener('error', done);
      resolve();
    };
    child.once('exit', done);
    child.once('error', done);
    timer = setTimeout(done, 1_000);
  });
}

// The profile is removed only after the browser is gone, but "gone" is a race
// the kernel arbitrates: a helper that received SIGKILL a microsecond ago can
// still have the directory open. Retry briefly before giving up, so a teardown
// that is merely slow stops being reported as a failed one — and still throw if
// the directory genuinely survives, because a leaked profile is state the next
// run would inherit.
async function removeProfile(profileDir, attempts = 20) {
  for (let attempt = 1; ; attempt += 1) {
    try {
      rmSync(profileDir, { recursive: true, force: true });
      return;
    } catch (error) {
      if (attempt >= attempts) throw error;
      await sleep(50);
    }
  }
}

export async function launchChromeCdp({
  chromePath,
  chromeArgs = [],
  cdpPort,
  profilePrefix = 'plombir-git-browser-smoke-',
  startupTimeoutMs = 12_000,
}) {
  const requestedPort = requestedCdpPort(cdpPort);
  if (!Number.isFinite(startupTimeoutMs) || startupTimeoutMs <= 0) {
    throw new Error(`startupTimeoutMs must be positive, got ${startupTimeoutMs}`);
  }

  const profileDir = mkdtempSync(join(tmpdir(), profilePrefix));
  let child = null;
  let state = null;
  let cleaned = false;

  const cleanup = async () => {
    if (cleaned) return;
    cleaned = true;
    await stopChild(child, state);
    await removeProfile(profileDir);
  };

  try {
    child = spawn(chromePath, [
      `--remote-debugging-port=${requestedPort}`,
      `--user-data-dir=${profileDir}`,
      ...chromeArgs,
    ], { stdio: 'ignore', detached: true });
    state = watchChild(child, chromePath);

    const deadline = Date.now() + startupTimeoutMs;
    const actualPort = requestedPort === 0
      ? await publishedCdpPort(profileDir, state, deadline)
      : requestedPort;
    const cdpRoot = `http://127.0.0.1:${actualPort}`;
    await waitForDebugger(cdpRoot, state, deadline);

    return { child, cdpRoot, port: actualPort, profileDir, requestedPort, cleanup };
  } catch (error) {
    await cleanup();
    throw error;
  }
}

/// Open one tab after its CDP observers are attached.
///
/// Browser checks used to grow a fresh WebSocket implementation every time.
/// The launcher's ownership boundary already lives here, so the tab transport
/// does too: command deadlines, pending-waiter rejection and target cleanup are
/// now one contract. Callers still own event policy through `onEvent` — an
/// admin smoke may tolerate an expected 403 while an end-to-end journey may
/// treat the same response as a failure.
export async function openChromeTab({
  cdpRoot,
  url = 'about:blank',
  commandTimeoutMs = 10_000,
  lifecycleEvents = false,
  onEvent = () => {},
}) {
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(String(cdpRoot))) {
    throw new Error(`cdpRoot must be a loopback Chrome endpoint, got ${JSON.stringify(cdpRoot)}`);
  }
  if (!Number.isFinite(commandTimeoutMs) || commandTimeoutMs <= 0) {
    throw new Error(`commandTimeoutMs must be positive, got ${commandTimeoutMs}`);
  }

  const response = await fetch(`${cdpRoot}/json/new?${encodeURIComponent(url)}`, { method: 'PUT' });
  const text = await response.text();
  let target;
  try { target = JSON.parse(text); } catch {
    throw new Error(`Chrome could not open a tab: HTTP ${response.status} ${text.slice(0, 120)}`);
  }
  if (!target?.id || !target.webSocketDebuggerUrl) {
    throw new Error(`Chrome returned no debugger target for ${url}`);
  }

  const ws = new WebSocket(target.webSocketDebuggerUrl);
  const pending = new Map();
  const eventErrors = [];
  let messageId = 0;
  let closing = false;

  const failPending = (error) => {
    for (const waiter of pending.values()) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
    pending.clear();
  };

  const send = (method, params = {}) => new Promise((resolveSend, rejectSend) => {
    const id = ++messageId;
    const timer = setTimeout(() => {
      if (!pending.delete(id)) return;
      rejectSend(new Error(`CDP command timed out after ${commandTimeoutMs} ms: ${method}`));
    }, commandTimeoutMs);
    pending.set(id, { resolve: resolveSend, reject: rejectSend, timer });
    try {
      ws.send(JSON.stringify({ id, method, params }));
    } catch (error) {
      clearTimeout(timer);
      pending.delete(id);
      rejectSend(error);
    }
  });

  const close = async () => {
    if (closing) return;
    closing = true;
    try { await fetch(`${cdpRoot}/json/close/${target.id}`); } catch {}
    failPending(new Error('Chrome tab closed'));
    ws.close();
  };

  await new Promise((resolveOpen, rejectOpen) => {
    ws.addEventListener('message', (event) => {
      let payload;
      try { payload = JSON.parse(event.data); } catch (error) {
        const protocolError = new Error(`Chrome tab sent malformed CDP data: ${error.message}`);
        eventErrors.push(protocolError);
        failPending(protocolError);
        return;
      }
      if (payload.id && pending.has(payload.id)) {
        const waiter = pending.get(payload.id);
        pending.delete(payload.id);
        clearTimeout(waiter.timer);
        if (payload.error) waiter.reject(new Error(payload.error.message || 'CDP error'));
        else waiter.resolve(payload.result || payload);
        return;
      }
      try { onEvent(payload); } catch (error) {
        eventErrors.push(error instanceof Error ? error : new Error(String(error)));
      }
    });
    ws.addEventListener('error', () => {
      const error = new Error('Chrome tab WebSocket failed');
      failPending(error);
      if (!closing) rejectOpen(error);
    });
    ws.addEventListener('close', () => {
      const error = new Error('Chrome tab WebSocket closed');
      failPending(error);
      if (!closing) rejectOpen(error);
    });
    ws.addEventListener('open', async () => {
      try {
        await send('Page.enable');
        if (lifecycleEvents) await send('Page.setLifecycleEventsEnabled', { enabled: true });
        await send('Runtime.enable');
        await send('Log.enable');
        await send('Network.enable');
        resolveOpen();
      } catch (error) {
        ws.close();
        rejectOpen(error);
      }
    });
  });

  return { id: target.id, send, close, eventErrors };
}
