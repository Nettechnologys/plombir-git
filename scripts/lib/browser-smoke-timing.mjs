const DEFAULT_CDP_COMMAND_TIMEOUT_MS = 10_000;
const DEFAULT_PAGE_LOAD_TIMEOUT_MS = 20_000;
const DEFAULT_REDIRECT_TIMEOUT_MS = 8_000;
const DEFAULT_JOURNEY_STARTUP_TIMEOUT_MS = 20_000;
const DEFAULT_JOURNEY_CDP_COMMAND_TIMEOUT_MS = 10_000;
const DEFAULT_JOURNEY_UI_WAIT_TIMEOUT_MS = 15_000;

function configuredValue(env, name) {
  const raw = env[name];
  return raw === undefined || String(raw).trim() === '' ? undefined : raw;
}

function positiveTimeout(name, raw, fallback) {
  const value = Number(raw ?? fallback);
  if (!Number.isInteger(value) || value <= 0) {
    throw new Error(`${name} must be a positive integer, got ${JSON.stringify(raw)}`);
  }
  return value;
}

function configuredTimeout(env, name, legacyName, fallback) {
  const configured = configuredValue(env, name);
  if (configured !== undefined) return positiveTimeout(name, configured, fallback);

  const legacy = configuredValue(env, legacyName);
  if (legacy !== undefined) return positiveTimeout(legacyName, legacy, fallback);

  return fallback;
}

export function browserAdminTimeouts(env = process.env) {
  // WAIT_MS predates the split. Keep it as a fallback so existing operator
  // invocations do not change meaning, while defaults and new overrides own
  // one budget each.
  const legacy = configuredValue(env, 'WAIT_MS');
  return {
    cdpCommandMs: positiveTimeout(
      'CDP_COMMAND_TIMEOUT_MS',
      configuredValue(env, 'CDP_COMMAND_TIMEOUT_MS') ?? legacy,
      DEFAULT_CDP_COMMAND_TIMEOUT_MS,
    ),
    pageLoadMs: positiveTimeout(
      'PAGE_LOAD_TIMEOUT_MS',
      configuredValue(env, 'PAGE_LOAD_TIMEOUT_MS') ?? legacy,
      DEFAULT_PAGE_LOAD_TIMEOUT_MS,
    ),
    redirectMs: positiveTimeout(
      'REDIRECT_TIMEOUT_MS',
      configuredValue(env, 'REDIRECT_TIMEOUT_MS') ?? legacy,
      DEFAULT_REDIRECT_TIMEOUT_MS,
    ),
  };
}

export function firstUserJourneyTimeouts(env = process.env) {
  // JOURNEY_WAIT_MS predates the split. An explicit legacy value must keep its
  // old meaning for existing invocations, while defaults and new overrides own
  // one failure boundary each.
  return {
    startupMs: configuredTimeout(
      env,
      'JOURNEY_STARTUP_TIMEOUT_MS',
      'JOURNEY_WAIT_MS',
      DEFAULT_JOURNEY_STARTUP_TIMEOUT_MS,
    ),
    cdpCommandMs: configuredTimeout(
      env,
      'JOURNEY_CDP_COMMAND_TIMEOUT_MS',
      'JOURNEY_WAIT_MS',
      DEFAULT_JOURNEY_CDP_COMMAND_TIMEOUT_MS,
    ),
    uiWaitMs: configuredTimeout(
      env,
      'JOURNEY_UI_WAIT_TIMEOUT_MS',
      'JOURNEY_WAIT_MS',
      DEFAULT_JOURNEY_UI_WAIT_TIMEOUT_MS,
    ),
  };
}

export function withCdpCommandTimeout({ method, timeoutMs, run, onTimeout = () => {} }) {
  if (typeof method !== 'string' || method.length === 0) {
    throw new Error('CDP command method is required');
  }
  if (!Number.isInteger(timeoutMs) || timeoutMs <= 0) {
    throw new Error(`CDP command timeout must be a positive integer, got ${timeoutMs}`);
  }
  if (typeof run !== 'function') throw new Error('CDP command run callback is required');

  return new Promise((resolve, reject) => {
    let settled = false;
    const settle = (outcome, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (outcome === 'resolve') resolve(value);
      else reject(value);
    };

    const timer = setTimeout(() => {
      if (settled) return;
      try {
        onTimeout();
      } catch (error) {
        settle(
          'reject',
          new Error(`CDP command timeout cleanup failed for ${method}: ${error.message}`),
        );
        return;
      }
      settle(
        'reject',
        new Error(`CDP command timed out after ${timeoutMs} ms: ${method}`),
      );
    }, timeoutMs);

    Promise.resolve()
      .then(run)
      .then(
        (value) => settle('resolve', value),
        (error) => settle('reject', error),
      );
  });
}

export function createEventWaiters({ eventName, timeoutMs }) {
  const waiters = new Set();

  function settle(waiter, outcome, value) {
    if (!waiters.delete(waiter)) return;
    clearTimeout(waiter.timer);
    waiter[outcome](value);
  }

  return {
    wait(description) {
      return new Promise((resolve, reject) => {
        const waiter = { description, resolve, reject, timer: null };
        waiter.timer = setTimeout(() => {
          settle(
            waiter,
            'reject',
            new Error(`${eventName} timed out after ${timeoutMs} ms while waiting for ${description}`),
          );
        }, timeoutMs);
        waiters.add(waiter);
      });
    },

    resolveAll() {
      for (const waiter of [...waiters]) settle(waiter, 'resolve');
    },

    rejectAll(error) {
      const reason = error instanceof Error ? error.message : String(error);
      for (const waiter of [...waiters]) {
        settle(
          waiter,
          'reject',
          new Error(`${eventName} aborted while waiting for ${waiter.description}: ${reason}`),
        );
      }
    },
  };
}

function printable(value) {
  const encoded = JSON.stringify(value);
  return encoded === undefined ? String(value) : encoded;
}

export async function waitForValue({
  read,
  accept,
  description,
  timeoutMs,
  pollIntervalMs = 100,
  now = Date.now,
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
}) {
  const deadline = now() + timeoutMs;
  let lastValue = await read();

  while (!accept(lastValue) && now() < deadline) {
    await sleep(Math.min(pollIntervalMs, Math.max(0, deadline - now())));
    lastValue = await read();
  }

  if (accept(lastValue)) return lastValue;
  throw new Error(
    `timed out after ${timeoutMs} ms waiting for ${description}; last value=${printable(lastValue)}`,
  );
}
