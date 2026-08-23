const DEFAULT_CDP_COMMAND_TIMEOUT_MS = 10_000;
const DEFAULT_PAGE_LOAD_TIMEOUT_MS = 20_000;
const DEFAULT_REDIRECT_TIMEOUT_MS = 8_000;

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
