#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';

function getCliArg(name, fallback = '') {
  const args = process.argv.slice(2);
  const withEquals = `--${name}=`;
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === `--${name}` && i + 1 < args.length) {
      return args[i + 1];
    }

    if (arg.startsWith(withEquals)) {
      return arg.slice(withEquals.length);
    }
  }

  return fallback;
}

const ROOT = process.cwd();
const BACKEND_URL = (process.env.BACKEND_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const FRONTEND_URL_CANDIDATES = process.env.FRONTEND_URL
  ? [process.env.FRONTEND_URL]
  : ['http://127.0.0.1:4173', 'http://127.0.0.1:5173'];
const SKIP_BACKEND_TESTS = /^(1|true|yes)$/i.test(process.env.SKIP_BACKEND_TESTS || '');
const SKIP_FRONTEND_STATIC = /^(1|true|yes)$/i.test(process.env.SKIP_FRONTEND_STATIC || '');
const SKIP_RUNTIME_SMOKES = /^(1|true|yes)$/i.test(process.env.SKIP_RUNTIME_SMOKES || '');
const FULL_REGRESSION_ONLY = (process.env.FULL_REGRESSION_ONLY || '').trim().toLowerCase();
const REGRESSION_TIMEOUT_MS = Number.parseInt(process.env.REGRESSION_TIMEOUT_MS || '', 10);
const REGRESSION_RETRIES = parsePositiveInt(process.env.REGRESSION_RETRIES, 0);
const REGRESSION_RETRY_DELAY_MS = parsePositiveInt(process.env.REGRESSION_RETRY_DELAY_MS, 2000);
const REGRESSION_RETRY_BACKOFF_MS = parsePositiveFloat(process.env.REGRESSION_RETRY_BACKOFF_MS, 1);
const REGRESSION_RETRY_MAX_DELAY_MS = parsePositiveInt(process.env.REGRESSION_RETRY_MAX_DELAY_MS, 30000);

function parseTimeout(value) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : null;
}

function parsePositiveInt(value, fallback = 0) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function parsePositiveFloat(value, fallback = 1) {
  const parsed = Number.parseFloat(value || '');
  return Number.isFinite(parsed) && parsed >= 1 ? parsed : fallback;
}

const DEFAULT_TIMEOUT_MS = parseTimeout(process.env.REGRESSION_TIMEOUT_MS);
const BACKEND_TEST_TIMEOUT_MS = parseTimeout(process.env.BACKEND_TEST_TIMEOUT_MS) ?? DEFAULT_TIMEOUT_MS;
const FRONTEND_STATIC_TIMEOUT_MS = parseTimeout(process.env.FRONTEND_STATIC_TIMEOUT_MS) ?? DEFAULT_TIMEOUT_MS;
const RUNTIME_TIMEOUT_MS = parseTimeout(process.env.RUNTIME_TIMEOUT_MS) ?? DEFAULT_TIMEOUT_MS;
const BACKEND_TEST_RETRIES = parsePositiveInt(process.env.BACKEND_TEST_RETRIES, REGRESSION_RETRIES);
const FRONTEND_STATIC_RETRIES = parsePositiveInt(process.env.FRONTEND_STATIC_RETRIES, REGRESSION_RETRIES);
const RUNTIME_RETRIES = parsePositiveInt(process.env.RUNTIME_RETRIES, REGRESSION_RETRIES);
const BACKEND_TEST_RG_HTTP_RETRIES = parsePositiveInt(process.env.BACKEND_TEST_RG_HTTP_RETRIES, BACKEND_TEST_RETRIES);
const BACKEND_TEST_WORKSPACE_RETRIES = parsePositiveInt(process.env.BACKEND_TEST_WORKSPACE_RETRIES, BACKEND_TEST_RETRIES);
const FRONTEND_CHECK_RETRIES = parsePositiveInt(process.env.FRONTEND_CHECK_RETRIES, FRONTEND_STATIC_RETRIES);
const FRONTEND_BUILD_RETRIES = parsePositiveInt(process.env.FRONTEND_BUILD_RETRIES, FRONTEND_STATIC_RETRIES);
const OPENAPI_SMOKE_RETRIES = parsePositiveInt(process.env.OPENAPI_SMOKE_RETRIES, RUNTIME_RETRIES);
const CLIENT_CONTRACT_RETRIES = parsePositiveInt(process.env.CLIENT_CONTRACT_RETRIES, RUNTIME_RETRIES);
const FRONTEND_BACKEND_SMOKE_RETRIES = parsePositiveInt(process.env.FRONTEND_BACKEND_SMOKE_RETRIES, RUNTIME_RETRIES);
const CONSOLE_SMOKE_RETRIES = parsePositiveInt(process.env.CONSOLE_SMOKE_RETRIES, RUNTIME_RETRIES);
const ADMIN_BROWSER_SMOKE_RETRIES = parsePositiveInt(process.env.ADMIN_BROWSER_SMOKE_RETRIES, RUNTIME_RETRIES);
const REGRESSION_REPORT_FILE = process.env.REGRESSION_REPORT_FILE || '';
const BACKEND_TEST_RG_HTTP_RETRY_BACKOFF_MS = parsePositiveFloat(process.env.BACKEND_TEST_RG_HTTP_RETRY_BACKOFF_MS, REGRESSION_RETRY_BACKOFF_MS);
const BACKEND_TEST_WORKSPACE_RETRY_BACKOFF_MS = parsePositiveFloat(process.env.BACKEND_TEST_WORKSPACE_RETRY_BACKOFF_MS, REGRESSION_RETRY_BACKOFF_MS);
const FRONTEND_CHECK_RETRY_BACKOFF_MS = parsePositiveFloat(process.env.FRONTEND_CHECK_RETRY_BACKOFF_MS, REGRESSION_RETRY_BACKOFF_MS);
const FRONTEND_BUILD_RETRY_BACKOFF_MS = parsePositiveFloat(process.env.FRONTEND_BUILD_RETRY_BACKOFF_MS, REGRESSION_RETRY_BACKOFF_MS);
const OPENAPI_SMOKE_RETRY_BACKOFF_MS = parsePositiveFloat(process.env.OPENAPI_SMOKE_RETRY_BACKOFF_MS, REGRESSION_RETRY_BACKOFF_MS);
const CLIENT_CONTRACT_RETRY_BACKOFF_MS = parsePositiveFloat(process.env.CLIENT_CONTRACT_RETRY_BACKOFF_MS, REGRESSION_RETRY_BACKOFF_MS);
const FRONTEND_BACKEND_SMOKE_RETRY_BACKOFF_MS = parsePositiveFloat(process.env.FRONTEND_BACKEND_SMOKE_RETRY_BACKOFF_MS, REGRESSION_RETRY_BACKOFF_MS);
const CONSOLE_SMOKE_RETRY_BACKOFF_MS = parsePositiveFloat(process.env.CONSOLE_SMOKE_RETRY_BACKOFF_MS, REGRESSION_RETRY_BACKOFF_MS);
const ADMIN_BROWSER_SMOKE_RETRY_BACKOFF_MS = parsePositiveFloat(process.env.ADMIN_BROWSER_SMOKE_RETRY_BACKOFF_MS, REGRESSION_RETRY_BACKOFF_MS);
const BACKEND_TEST_RG_HTTP_TIMEOUT_MS = parseTimeout(process.env.BACKEND_TEST_RG_HTTP_TIMEOUT_MS) ?? BACKEND_TEST_TIMEOUT_MS;
const BACKEND_TEST_WORKSPACE_TIMEOUT_MS = parseTimeout(process.env.BACKEND_TEST_WORKSPACE_TIMEOUT_MS) ?? BACKEND_TEST_TIMEOUT_MS;
const FRONTEND_CHECK_TIMEOUT_MS = parseTimeout(process.env.FRONTEND_CHECK_TIMEOUT_MS) ?? FRONTEND_STATIC_TIMEOUT_MS;
const FRONTEND_BUILD_TIMEOUT_MS = parseTimeout(process.env.FRONTEND_BUILD_TIMEOUT_MS) ?? FRONTEND_STATIC_TIMEOUT_MS;
const OPENAPI_SMOKE_TIMEOUT_MS = parseTimeout(process.env.OPENAPI_SMOKE_TIMEOUT_MS) ?? RUNTIME_TIMEOUT_MS;
const CLIENT_CONTRACT_TIMEOUT_MS = parseTimeout(process.env.CLIENT_CONTRACT_TIMEOUT_MS) ?? RUNTIME_TIMEOUT_MS;
const FRONTEND_BACKEND_SMOKE_TIMEOUT_MS = parseTimeout(process.env.FRONTEND_BACKEND_SMOKE_TIMEOUT_MS) ?? RUNTIME_TIMEOUT_MS;
const CONSOLE_SMOKE_TIMEOUT_MS = parseTimeout(process.env.CONSOLE_SMOKE_TIMEOUT_MS) ?? RUNTIME_TIMEOUT_MS;
const ADMIN_BROWSER_SMOKE_TIMEOUT_MS = parseTimeout(process.env.ADMIN_BROWSER_SMOKE_TIMEOUT_MS) ?? RUNTIME_TIMEOUT_MS;
const BACKEND_TEST_RG_HTTP_RETRY_MAX_DELAY_MS = parsePositiveInt(process.env.BACKEND_TEST_RG_HTTP_RETRY_MAX_DELAY_MS, REGRESSION_RETRY_MAX_DELAY_MS);
const BACKEND_TEST_WORKSPACE_RETRY_MAX_DELAY_MS = parsePositiveInt(process.env.BACKEND_TEST_WORKSPACE_RETRY_MAX_DELAY_MS, REGRESSION_RETRY_MAX_DELAY_MS);
const FRONTEND_CHECK_RETRY_MAX_DELAY_MS = parsePositiveInt(process.env.FRONTEND_CHECK_RETRY_MAX_DELAY_MS, REGRESSION_RETRY_MAX_DELAY_MS);
const FRONTEND_BUILD_RETRY_MAX_DELAY_MS = parsePositiveInt(process.env.FRONTEND_BUILD_RETRY_MAX_DELAY_MS, REGRESSION_RETRY_MAX_DELAY_MS);
const OPENAPI_SMOKE_RETRY_MAX_DELAY_MS = parsePositiveInt(process.env.OPENAPI_SMOKE_RETRY_MAX_DELAY_MS, REGRESSION_RETRY_MAX_DELAY_MS);
const CLIENT_CONTRACT_RETRY_MAX_DELAY_MS = parsePositiveInt(process.env.CLIENT_CONTRACT_RETRY_MAX_DELAY_MS, REGRESSION_RETRY_MAX_DELAY_MS);
const FRONTEND_BACKEND_SMOKE_RETRY_MAX_DELAY_MS = parsePositiveInt(process.env.FRONTEND_BACKEND_SMOKE_RETRY_MAX_DELAY_MS, REGRESSION_RETRY_MAX_DELAY_MS);
const CONSOLE_SMOKE_RETRY_MAX_DELAY_MS = parsePositiveInt(process.env.CONSOLE_SMOKE_RETRY_MAX_DELAY_MS, REGRESSION_RETRY_MAX_DELAY_MS);
const ADMIN_BROWSER_SMOKE_RETRY_MAX_DELAY_MS = parsePositiveInt(process.env.ADMIN_BROWSER_SMOKE_RETRY_MAX_DELAY_MS, REGRESSION_RETRY_MAX_DELAY_MS);
const CLI_REPORT_FORMAT = getCliArg('report-format', '').trim().toLowerCase();
const REGRESSION_REPORT_FORMAT = (() => {
  const requested = (CLI_REPORT_FORMAT || process.env.REGRESSION_REPORT_FORMAT || '').toLowerCase();
  if (!requested && REGRESSION_REPORT_FILE.toLowerCase().endsWith('.md')) {
    return 'md';
  }
  if (requested === 'json' || requested === 'md') {
    return requested;
  }
  if (requested) {
    console.warn(`⚠️  Unsupported regression report format: ${requested}, falling back to json`);
  }

  return 'json';
})();
const EFFECTIVE_TIMEOUTS = {
  backend: formatTimeout('backend', BACKEND_TEST_TIMEOUT_MS),
  frontend: formatTimeout('frontend', FRONTEND_STATIC_TIMEOUT_MS),
  runtime: formatTimeout('runtime', RUNTIME_TIMEOUT_MS),
  all: formatTimeout('all', DEFAULT_TIMEOUT_MS),
  backendRgHttp: formatTimeout('backend rg-http', BACKEND_TEST_RG_HTTP_TIMEOUT_MS),
  backendWorkspace: formatTimeout('backend workspace', BACKEND_TEST_WORKSPACE_TIMEOUT_MS),
  frontendCheck: formatTimeout('frontend check', FRONTEND_CHECK_TIMEOUT_MS),
  frontendBuild: formatTimeout('frontend build', FRONTEND_BUILD_TIMEOUT_MS),
  openapiSmoke: formatTimeout('openapi smoke', OPENAPI_SMOKE_TIMEOUT_MS),
  clientContract: formatTimeout('client contract', CLIENT_CONTRACT_TIMEOUT_MS),
  frontendBackendSmoke: formatTimeout('frontend-backend smoke', FRONTEND_BACKEND_SMOKE_TIMEOUT_MS),
  consoleSmoke: formatTimeout('console smoke', CONSOLE_SMOKE_TIMEOUT_MS),
  adminBrowserSmoke: formatTimeout('admin-browser smoke', ADMIN_BROWSER_SMOKE_TIMEOUT_MS),
};
const EFFECTIVE_RETRIES = {
  backend: BACKEND_TEST_RETRIES,
  frontend: FRONTEND_STATIC_RETRIES,
  runtime: RUNTIME_RETRIES,
  all: REGRESSION_RETRIES,
  backendRgHttp: BACKEND_TEST_RG_HTTP_RETRIES,
  backendWorkspace: BACKEND_TEST_WORKSPACE_RETRIES,
  frontendCheck: FRONTEND_CHECK_RETRIES,
  frontendBuild: FRONTEND_BUILD_RETRIES,
  openapiSmoke: OPENAPI_SMOKE_RETRIES,
  clientContract: CLIENT_CONTRACT_RETRIES,
  frontendBackendSmoke: FRONTEND_BACKEND_SMOKE_RETRIES,
  consoleSmoke: CONSOLE_SMOKE_RETRIES,
  adminBrowserSmoke: ADMIN_BROWSER_SMOKE_RETRIES,
  backendRgHttpBackoff: BACKEND_TEST_RG_HTTP_RETRY_BACKOFF_MS,
  backendWorkspaceBackoff: BACKEND_TEST_WORKSPACE_RETRY_BACKOFF_MS,
  frontendCheckBackoff: FRONTEND_CHECK_RETRY_BACKOFF_MS,
  frontendBuildBackoff: FRONTEND_BUILD_RETRY_BACKOFF_MS,
  openapiSmokeBackoff: OPENAPI_SMOKE_RETRY_BACKOFF_MS,
  clientContractBackoff: CLIENT_CONTRACT_RETRY_BACKOFF_MS,
  frontendBackendSmokeBackoff: FRONTEND_BACKEND_SMOKE_RETRY_BACKOFF_MS,
  consoleSmokeBackoff: CONSOLE_SMOKE_RETRY_BACKOFF_MS,
  adminBrowserSmokeBackoff: ADMIN_BROWSER_SMOKE_RETRY_BACKOFF_MS,
  backendRgHttpMaxDelay: BACKEND_TEST_RG_HTTP_RETRY_MAX_DELAY_MS,
  backendWorkspaceMaxDelay: BACKEND_TEST_WORKSPACE_RETRY_MAX_DELAY_MS,
  frontendCheckMaxDelay: FRONTEND_CHECK_RETRY_MAX_DELAY_MS,
  frontendBuildMaxDelay: FRONTEND_BUILD_RETRY_MAX_DELAY_MS,
  openapiSmokeMaxDelay: OPENAPI_SMOKE_RETRY_MAX_DELAY_MS,
  clientContractMaxDelay: CLIENT_CONTRACT_RETRY_MAX_DELAY_MS,
  frontendBackendSmokeMaxDelay: FRONTEND_BACKEND_SMOKE_RETRY_MAX_DELAY_MS,
  consoleSmokeMaxDelay: CONSOLE_SMOKE_RETRY_MAX_DELAY_MS,
  adminBrowserSmokeMaxDelay: ADMIN_BROWSER_SMOKE_RETRY_MAX_DELAY_MS,
};

const STEP_RESULTS = [];

function getRunTimeout(scope, options = {}) {
  if (Number.isFinite(options.timeout)) {
    return options.timeout;
  }

  if (scope === 'backend') return BACKEND_TEST_TIMEOUT_MS;
  if (scope === 'frontend') return FRONTEND_STATIC_TIMEOUT_MS;
  if (scope === 'runtime') return RUNTIME_TIMEOUT_MS;
  return DEFAULT_TIMEOUT_MS;
}

function getRunRetries(scope, options = {}) {
  if (Number.isFinite(options.retries)) {
    return options.retries;
  }

  if (scope === 'backend') return BACKEND_TEST_RETRIES;
  if (scope === 'frontend') return FRONTEND_STATIC_RETRIES;
  if (scope === 'runtime') return RUNTIME_RETRIES;
  return REGRESSION_RETRIES;
}

function formatTimeout(scope, timeout) {
  return timeout == null ? 'no-timeout' : `${timeout}ms`;
}

function isScopeEnabled(scope) {
  if (!FULL_REGRESSION_ONLY || FULL_REGRESSION_ONLY === 'all') return true;
  return FULL_REGRESSION_ONLY === scope;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function getRetryDelay(attempt, options = {}) {
  const baseDelay = Number.isFinite(options.baseDelay) ? options.baseDelay : REGRESSION_RETRY_DELAY_MS;
  const backoff = Number.isFinite(options.backoff) ? options.backoff : REGRESSION_RETRY_BACKOFF_MS;
  const maxDelay = Number.isFinite(options.maxDelay) ? options.maxDelay : REGRESSION_RETRY_MAX_DELAY_MS;
  return Math.max(
    0,
    Math.min(maxDelay, Math.floor(baseDelay * backoff ** attempt)),
  );
}

function run(label, command, args, options = {}) {
  const env = { ...process.env, ...(options.env || {}) };
  const scope = options.scope || 'all';
  const timeout = getRunTimeout(scope, options);
  console.log(`⏱️  [${scope}] timeout=${formatTimeout(scope, timeout)}`);
  const result = spawnSync(command, args, {
    cwd: options.cwd || ROOT,
    env,
    stdio: 'inherit',
    timeout,
    shell: false,
  });

  if (result.error) {
    console.error(`❌ ${label} failed to start: ${result.error.message}`);
    return false;
  }

  if (result.status !== 0) {
    console.error(`❌ ${label} failed (exit ${result.status})`);
    return false;
  }

  console.log(`✅ ${label}`);
  return true;
}

async function runWithRetry(label, command, args, options = {}) {
  const scope = options.scope || 'all';
  const maxRetries = getRunRetries(scope, options);
  const delayConfig = {
    baseDelay: Number.isFinite(options.retryDelayMs) ? options.retryDelayMs : REGRESSION_RETRY_DELAY_MS,
    backoff: Number.isFinite(options.retryBackoffMs) ? options.retryBackoffMs : REGRESSION_RETRY_BACKOFF_MS,
    maxDelay: Number.isFinite(options.retryMaxDelayMs) ? options.retryMaxDelayMs : REGRESSION_RETRY_MAX_DELAY_MS,
  };
  const stepStartedAt = Date.now();
  const attempts = [];
  let attempt = 0;

  while (attempt <= maxRetries) {
    const currentAttempt = attempt + 1;
    const attemptLabel = `${label} [${currentAttempt}/${maxRetries + 1}]`;
    const attemptStartedAt = Date.now();
    const ok = run(attemptLabel, command, args, options);
    attempts.push({
      attempt: currentAttempt,
      startedAt: new Date(attemptStartedAt).toISOString(),
      durationMs: Date.now() - attemptStartedAt,
      status: ok ? 'passed' : 'failed',
    });
    if (ok) {
      STEP_RESULTS.push({
        label,
        scope,
        command: [command, ...args].join(' '),
        status: 'passed',
        configuredRetries: maxRetries,
        actualRetries: Math.max(0, currentAttempt - 1),
        timeoutMs: getRunTimeout(scope, options),
        retryDelayMs: delayConfig.baseDelay,
        retryBackoffMs: delayConfig.backoff,
        retryMaxDelayMs: delayConfig.maxDelay,
        attempts,
        startedAt: new Date(stepStartedAt).toISOString(),
        durationMs: Date.now() - stepStartedAt,
      });
      return true;
    }

    if (attempt === maxRetries) {
      console.error(`❌ ${label} exceeded the retry limit (${maxRetries})`);
      return false;
    }

    const delay = getRetryDelay(attempt, delayConfig);
    console.warn(`⚠️ ${label} failed, retrying in ${delay}ms (attempt ${currentAttempt})`);
    if (delay > 0) {
      await sleep(delay);
    }
    attempt += 1;
  }

  STEP_RESULTS.push({
    label,
    scope,
    command: [command, ...args].join(' '),
    status: 'failed',
    configuredRetries: maxRetries,
    actualRetries: maxRetries,
    timeoutMs: getRunTimeout(scope, options),
    retryDelayMs: delayConfig.baseDelay,
    retryBackoffMs: delayConfig.backoff,
    retryMaxDelayMs: delayConfig.maxDelay,
    attempts,
    startedAt: new Date(stepStartedAt).toISOString(),
    durationMs: Date.now() - stepStartedAt,
  });

  return false;
}

function canPing(url) {
  return fetch(url, { method: 'GET' })
    .then((res) => res.ok)
    .catch(() => false);
}

async function firstReachableUrl(urls) {
  for (const url of urls) {
    const normalized = url.replace(/\/$/, '');
    if (await canPing(`${normalized}/`)) return normalized;
  }
  return null;
}

function saveRegressionReport(summary) {
  if (!REGRESSION_REPORT_FILE) return;

  const payload = {
    generatedAt: new Date().toISOString(),
    summary,
    config: {
      backendUrl: BACKEND_URL,
      frontendCandidates: FRONTEND_URL_CANDIDATES,
      reportFormat: REGRESSION_REPORT_FORMAT,
      timeouts: EFFECTIVE_TIMEOUTS,
      retries: EFFECTIVE_RETRIES,
      retryDelayMs: REGRESSION_RETRY_DELAY_MS,
      retryBackoff: REGRESSION_RETRY_BACKOFF_MS,
      retryMaxDelayMs: REGRESSION_RETRY_MAX_DELAY_MS,
    },
    steps: STEP_RESULTS,
  };

  try {
    const report =
      REGRESSION_REPORT_FORMAT === 'md'
        ? buildMarkdownReport(payload)
        : JSON.stringify(payload, null, 2);
    writeFileSync(REGRESSION_REPORT_FILE, report, 'utf8');
    console.log(
      `✅ Wrote ${REGRESSION_REPORT_FORMAT.toUpperCase()} regression report: ${REGRESSION_REPORT_FILE}`,
    );
  } catch (error) {
    console.error(`❌ Failed to write regression report: ${error.message}`);
  }
}

function formatMarkdownRows(value, fallback = '-') {
  return value == null || value === '' ? fallback : String(value);
}

function escapeMarkdown(text) {
  return String(text ?? '').replace(/\\/g, '\\\\').replace(/\|/g, '\\|').replace(/\n/g, '<br>');
}

function buildMarkdownReport(payload) {
  const stepRows = payload.steps
    .map((step, index) => {
      const attempts = (step.attempts || [])
        .map((attempt) => `${attempt.attempt}:${attempt.status} (${attempt.durationMs}ms)`)
        .join(', ');
      return `| ${index + 1} | ${escapeMarkdown(formatMarkdownRows(step.label))} | ${escapeMarkdown(step.scope)} | ${escapeMarkdown(step.status)} | ${step.actualRetries}/${step.configuredRetries} | ${step.durationMs ?? 0} | ${escapeMarkdown(formatMarkdownRows(step.timeoutMs))} | ${escapeMarkdown(step.command)} | ${escapeMarkdown(attempts || 'none')} |`;
    })
    .join('\n');

  const failedSteps = payload.steps.filter((step) => step.status !== 'passed');
  const failedItems = failedSteps
    .map((step) => `- ${escapeMarkdown(step.label)} (scope: ${escapeMarkdown(step.scope)})`)
    .join('\n');

  return `# Full Interface Regression Report

## Generation Info

- Generated at: ${payload.generatedAt}
- Status: ${payload.summary.status}
- Started: ${payload.summary.startedAt}
- Ended: ${payload.summary.endedAt}

## Summary

- total: ${payload.summary.total}
- passed: ${payload.summary.passed}
- failed: ${payload.summary.failed}
- executed: ${payload.summary.executed}
- skipped: ${payload.summary.skipped}

## Configuration

### Backend and Frontend (Timeouts)

${Object.entries(payload.config.timeouts)
      .map(([key, value]) => `- ${escapeMarkdown(key)}: ${escapeMarkdown(formatMarkdownRows(value))}`)
      .join('\n')}

### Retry Parameters

${Object.entries(payload.config.retries)
      .map(([key, value]) => `- ${escapeMarkdown(key)}: ${escapeMarkdown(formatMarkdownRows(value))}`)
      .join('\n')}

### Backend Connection

- BACKEND_URL: ${escapeMarkdown(payload.config.backendUrl)}
- FRONTEND_CANDIDATES: ${escapeMarkdown(payload.config.frontendCandidates.join(', '))}

## Step Details

| # | Step | scope | Status | Actual/Configured retries | Duration(ms) | Timeout(ms) | Command | Retry details |
| - | - | - | - | - | - | - | - |
${stepRows || '| - | No steps executed | - | skipped | 0/0 | 0 | - | - | - |'}

## Failed Steps

${failedItems || '- No failed steps'}

`;
}

async function main() {
  const startedAt = new Date().toISOString();
  const allOk = [];
  const invalidScope = FULL_REGRESSION_ONLY && !['backend', 'frontend', 'runtime', 'all'].includes(FULL_REGRESSION_ONLY);
  if (invalidScope) {
    console.error(`❌ FULL_REGRESSION_ONLY not supported: ${FULL_REGRESSION_ONLY}`);
    console.error('Supported: backend | frontend | runtime | all');
    process.exit(1);
  }

  console.log('--- Regression timeout config ---');
  console.log(`Default (full pipeline): ${EFFECTIVE_TIMEOUTS.all}`);
  console.log(`Backend: ${EFFECTIVE_TIMEOUTS.backend}`);
  console.log(`Frontend static: ${EFFECTIVE_TIMEOUTS.frontend}`);
  console.log(`Runtime: ${EFFECTIVE_TIMEOUTS.runtime}`);
  console.log(`Backend rg-http: ${EFFECTIVE_TIMEOUTS.backendRgHttp}`);
  console.log(`Backend workspace: ${EFFECTIVE_TIMEOUTS.backendWorkspace}`);
  console.log(`Frontend check: ${EFFECTIVE_TIMEOUTS.frontendCheck}`);
  console.log(`Frontend build: ${EFFECTIVE_TIMEOUTS.frontendBuild}`);
  console.log(`openapi smoke: ${EFFECTIVE_TIMEOUTS.openapiSmoke}`);
  console.log(`Parameter alignment: ${EFFECTIVE_TIMEOUTS.clientContract}`);
  console.log(`Frontend/backend integration smoke: ${EFFECTIVE_TIMEOUTS.frontendBackendSmoke}`);
  console.log(`Page console smoke: ${EFFECTIVE_TIMEOUTS.consoleSmoke}`);
  console.log(`admin browser smoke: ${EFFECTIVE_TIMEOUTS.adminBrowserSmoke}`);
  console.log('--- Regression retry config ---');
  console.log(`Default retry count: ${EFFECTIVE_RETRIES.all}`);
  console.log(`Backend: ${EFFECTIVE_RETRIES.backend}`);
  console.log(`Frontend static: ${EFFECTIVE_RETRIES.frontend}`);
  console.log(`Runtime: ${EFFECTIVE_RETRIES.runtime}`);
  console.log(`Backend rg-http: ${EFFECTIVE_RETRIES.backendRgHttp}`);
  console.log(`Backend workspace: ${EFFECTIVE_RETRIES.backendWorkspace}`);
  console.log(`Frontend check: ${EFFECTIVE_RETRIES.frontendCheck}`);
  console.log(`Frontend build: ${EFFECTIVE_RETRIES.frontendBuild}`);
  console.log(`openapi smoke: ${EFFECTIVE_RETRIES.openapiSmoke}`);
  console.log(`Parameter alignment: ${EFFECTIVE_RETRIES.clientContract}`);
  console.log(`Frontend/backend integration smoke: ${EFFECTIVE_RETRIES.frontendBackendSmoke}`);
  console.log(`Page console smoke: ${EFFECTIVE_RETRIES.consoleSmoke}`);
  console.log(`admin browser smoke: ${EFFECTIVE_RETRIES.adminBrowserSmoke}`);
  console.log(`Backend rg-http retry backoff: ${EFFECTIVE_RETRIES.backendRgHttpBackoff}`);
  console.log(`Backend workspace retry backoff: ${EFFECTIVE_RETRIES.backendWorkspaceBackoff}`);
  console.log(`Frontend check retry backoff: ${EFFECTIVE_RETRIES.frontendCheckBackoff}`);
  console.log(`Frontend build retry backoff: ${EFFECTIVE_RETRIES.frontendBuildBackoff}`);
  console.log(`openapi smoke retry backoff: ${EFFECTIVE_RETRIES.openapiSmokeBackoff}`);
  console.log(`Parameter alignment retry backoff: ${EFFECTIVE_RETRIES.clientContractBackoff}`);
  console.log(`Frontend/backend integration smoke retry backoff: ${EFFECTIVE_RETRIES.frontendBackendSmokeBackoff}`);
  console.log(`Page console smoke retry backoff: ${EFFECTIVE_RETRIES.consoleSmokeBackoff}`);
  console.log(`Backend rg-http max retry interval: ${EFFECTIVE_RETRIES.backendRgHttpMaxDelay}ms`);
  console.log(`Backend workspace max retry interval: ${EFFECTIVE_RETRIES.backendWorkspaceMaxDelay}ms`);
  console.log(`Frontend check max retry interval: ${EFFECTIVE_RETRIES.frontendCheckMaxDelay}ms`);
  console.log(`Frontend build max retry interval: ${EFFECTIVE_RETRIES.frontendBuildMaxDelay}ms`);
  console.log(`openapi smoke max retry interval: ${EFFECTIVE_RETRIES.openapiSmokeMaxDelay}ms`);
  console.log(`Parameter alignment max retry interval: ${EFFECTIVE_RETRIES.clientContractMaxDelay}ms`);
  console.log(`Frontend/backend integration smoke max retry interval: ${EFFECTIVE_RETRIES.frontendBackendSmokeMaxDelay}ms`);
  console.log(`Page console smoke max retry interval: ${EFFECTIVE_RETRIES.consoleSmokeMaxDelay}ms`);
  console.log(`admin browser smoke max retry interval: ${EFFECTIVE_RETRIES.adminBrowserSmokeMaxDelay}ms`);
  console.log(`Retry interval: ${REGRESSION_RETRY_DELAY_MS}ms`);
  console.log(`Retry backoff: ${REGRESSION_RETRY_BACKOFF_MS}`);
  console.log(`Max retry interval: ${REGRESSION_RETRY_MAX_DELAY_MS}ms`);
  console.log(`Regression report format: ${REGRESSION_REPORT_FORMAT}`);

  const runBackendTests = isScopeEnabled('backend');
  const runFrontendStatic = isScopeEnabled('frontend');
  const runRuntime = isScopeEnabled('runtime');

  if (runBackendTests && !SKIP_BACKEND_TESTS) {
    console.log('=== Backend regression ===');
    allOk.push(await runWithRetry('cargo test (rg-http full interface tests)', 'cargo', ['test', '-p', 'rg-http', '--', '--nocapture'], {
      scope: 'backend',
      retries: BACKEND_TEST_RG_HTTP_RETRIES,
      timeout: BACKEND_TEST_RG_HTTP_TIMEOUT_MS,
      retryDelayMs: REGRESSION_RETRY_DELAY_MS,
      retryBackoffMs: BACKEND_TEST_RG_HTTP_RETRY_BACKOFF_MS,
      retryMaxDelayMs: BACKEND_TEST_RG_HTTP_RETRY_MAX_DELAY_MS,
    }));
    allOk.push(await runWithRetry('cargo test (workspace core regression)', 'cargo', ['test', '--workspace', '--', '--nocapture'], {
      scope: 'backend',
      retries: BACKEND_TEST_WORKSPACE_RETRIES,
      timeout: BACKEND_TEST_WORKSPACE_TIMEOUT_MS,
      retryDelayMs: REGRESSION_RETRY_DELAY_MS,
      retryBackoffMs: BACKEND_TEST_WORKSPACE_RETRY_BACKOFF_MS,
      retryMaxDelayMs: BACKEND_TEST_WORKSPACE_RETRY_MAX_DELAY_MS,
    }));
  } else {
    console.log('=== Backend regression ===');
    if (SKIP_BACKEND_TESTS) {
      console.log('⚠️ SKIP_BACKEND_TESTS=1, backend tests skipped');
    } else if (FULL_REGRESSION_ONLY && !runBackendTests) {
      console.log('⚠️ FULL_REGRESSION_ONLY set to', FULL_REGRESSION_ONLY, ', backend tests skipped');
    }
  }

  console.log('\n=== Frontend static regression ===');
  if (runFrontendStatic && !SKIP_FRONTEND_STATIC) {
    allOk.push(await runWithRetry('web npm run check', 'npm', ['run', 'check'], {
      cwd: `${ROOT}/web`,
      scope: 'frontend',
      retries: FRONTEND_CHECK_RETRIES,
      timeout: FRONTEND_CHECK_TIMEOUT_MS,
      retryDelayMs: REGRESSION_RETRY_DELAY_MS,
      retryBackoffMs: FRONTEND_CHECK_RETRY_BACKOFF_MS,
      retryMaxDelayMs: FRONTEND_CHECK_RETRY_MAX_DELAY_MS,
    }));
    allOk.push(await runWithRetry('web npm run build', 'npm', ['run', 'build'], {
      cwd: `${ROOT}/web`,
      scope: 'frontend',
      retries: FRONTEND_BUILD_RETRIES,
      timeout: FRONTEND_BUILD_TIMEOUT_MS,
      retryDelayMs: REGRESSION_RETRY_DELAY_MS,
      retryBackoffMs: FRONTEND_BUILD_RETRY_BACKOFF_MS,
      retryMaxDelayMs: FRONTEND_BUILD_RETRY_MAX_DELAY_MS,
    }));
  } else {
    console.log('⚠️ SKIP_FRONTEND_STATIC=1 or FULL_REGRESSION_ONLY set, skipped web npm run check/build');
  }

  const backendOk = await canPing(`${BACKEND_URL}/health`);
  const frontendUrl = await firstReachableUrl(FRONTEND_URL_CANDIDATES);
  const frontendOk = !!frontendUrl;

  if (backendOk && runRuntime && !SKIP_RUNTIME_SMOKES) {
    console.log('\n=== Backend runtime interface regression (backend must be reachable) ===');
    allOk.push(
      await runWithRetry(
        'openapi path-level interface smoke',
        'node',
        ['scripts/openapi-interface-smoke.mjs'],
        {
          env: { BACKEND_URL },
          scope: 'runtime',
          retries: OPENAPI_SMOKE_RETRIES,
          timeout: OPENAPI_SMOKE_TIMEOUT_MS,
          retryBackoffMs: OPENAPI_SMOKE_RETRY_BACKOFF_MS,
          retryMaxDelayMs: OPENAPI_SMOKE_RETRY_MAX_DELAY_MS,
        },
      ),
    );
    allOk.push(
      await runWithRetry(
        'Frontend/backend parameter alignment check (frontend client vs OpenAPI)',
        'node',
        ['scripts/api-client-contract-check.mjs'],
        {
          env: { BACKEND_URL },
          scope: 'runtime',
          retries: CLIENT_CONTRACT_RETRIES,
          timeout: CLIENT_CONTRACT_TIMEOUT_MS,
          retryBackoffMs: CLIENT_CONTRACT_RETRY_BACKOFF_MS,
          retryMaxDelayMs: CLIENT_CONTRACT_RETRY_MAX_DELAY_MS,
        },
      ),
    );
  }

  if (backendOk && frontendOk && runRuntime && !SKIP_RUNTIME_SMOKES) {
    console.log('\n=== Frontend runtime interface regression (backend + frontend both reachable) ===');
    allOk.push(
      await runWithRetry(
        'Frontend/backend integration basic reachability',
        'node',
        ['scripts/frontend-backend-smoke.mjs'],
        {
          env: { BACKEND_URL, FRONTEND_URL: frontendUrl },
          scope: 'runtime',
          retries: FRONTEND_BACKEND_SMOKE_RETRIES,
          timeout: FRONTEND_BACKEND_SMOKE_TIMEOUT_MS,
          retryBackoffMs: FRONTEND_BACKEND_SMOKE_RETRY_BACKOFF_MS,
          retryMaxDelayMs: FRONTEND_BACKEND_SMOKE_RETRY_MAX_DELAY_MS,
        },
      ),
    );
    allOk.push(
      await runWithRetry(
        'Frontend key-page runtime smoke (console/network)',
        'node',
        ['scripts/console-smoke.mjs'],
        {
          env: { BASE: frontendUrl },
          scope: 'runtime',
          retries: CONSOLE_SMOKE_RETRIES,
          timeout: CONSOLE_SMOKE_TIMEOUT_MS,
          retryBackoffMs: CONSOLE_SMOKE_RETRY_BACKOFF_MS,
          retryMaxDelayMs: CONSOLE_SMOKE_RETRY_MAX_DELAY_MS,
        },
      ),
    );
    allOk.push(
      await runWithRetry(
        'admin page browser smoke (authenticated and unauthenticated redirects)',
        'node',
        ['scripts/browser-admin-smoke.mjs'],
        {
          env: {
            BACKEND_URL,
            FRONTEND_URL: frontendUrl,
            ADMIN_TOKEN: process.env.ADMIN_TOKEN || process.env.ADMIN_JWT || process.env.ACCESS_TOKEN || '',
          },
          scope: 'runtime',
          retries: ADMIN_BROWSER_SMOKE_RETRIES,
          timeout: ADMIN_BROWSER_SMOKE_TIMEOUT_MS,
          retryBackoffMs: ADMIN_BROWSER_SMOKE_RETRY_BACKOFF_MS,
          retryMaxDelayMs: ADMIN_BROWSER_SMOKE_RETRY_MAX_DELAY_MS,
        },
      ),
    );
  } else {
    if (SKIP_RUNTIME_SMOKES || (FULL_REGRESSION_ONLY && !runRuntime)) {
      console.log('\n⚠️ SKIP_RUNTIME_SMOKES=1, runtime integration skipped');
    } else {
      console.log('\n⚠️ Skipping some runtime smokes:');
      if (!backendOk) console.log(`  - Backend not ready: ${BACKEND_URL}/health`);
      if (!frontendOk) console.log(`  - Frontend not ready: ${FRONTEND_URL_CANDIDATES.map((url) => `${url.replace(/\/$/, '')}/`).join(', ')}`);
      console.log('  Start the services, then set BACKEND_URL/FRONTEND_URL to run the full interface regression');
    }
  }

  const failed = allOk.some((v) => !v);
  const passedCount = allOk.filter(Boolean).length;
  const totalCount = allOk.length;
  const failedCount = totalCount - passedCount;
  saveRegressionReport({
    startedAt,
    endedAt: new Date().toISOString(),
    status: failed ? 'failed' : 'passed',
    passed: passedCount,
    failed: failedCount,
    total: totalCount,
    executed: STEP_RESULTS.length,
    skipped: totalCount - STEP_RESULTS.length,
  });

  if (failed) {
    console.error('\n❌ Full pipeline interface regression failed');
    process.exit(1);
  }

  console.log('\n✅ Full pipeline interface regression passed');
}

await main();
