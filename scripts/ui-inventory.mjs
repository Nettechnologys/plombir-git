#!/usr/bin/env node

// Derive the UI inventory: every control a person can act on, the API call
// behind it, the access level the router holds that call to, and whether any
// test in the repo touches it.
//
// Run: node scripts/ui-inventory.mjs [--json docs/ui-inventory.json] [--md docs/UI_INVENTORY.md]
//
// This is a *generator*, not a gate. It writes the artefact a gate can later be
// held to; keeping the two apart means regenerating after a real UI change is a
// one-line diff to review rather than an assertion to rewrite.
//
// Why derived and not written by hand: `docs/FEATURE_INVENTORY.md` is the hand-
// maintained twin and carries "последняя сверка с кодом: 2026-07-23" — 962
// commits ago, 64 of them in the router and 59 in `web/src`. A list a person
// has to remember to update is a list that is wrong.

import { readFileSync, writeFileSync } from 'node:fs';
import path, { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { loadRouteTable, loadMountedHandlers, stripRustComments } from './lib/rust-source.mjs';
import { OPAQUE_SEGMENT, productionTsSource } from './lib/ts-source.mjs';
import { applyUiAccessSweepCoverage, loadUiAccessSweepSpec } from './lib/ui-access-sweep.mjs';
import { collectFiles, parseApiSurface, parsePageInventory } from './lib/ui-surface.mjs';

const ROUTER = 'crates/rg-http/src/routes.rs';
const API_DIR = 'web/src/lib/api';
const ROUTES_DIR = 'web/src/routes';
const COMPONENT_DIR = 'web/src/lib/components';
const API_BASE = '/api/v1';
const ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)));

function arg(name, fallback) {
  const argv = process.argv.slice(2);
  const at = argv.indexOf(`--${name}`);
  return at !== -1 && argv[at + 1] ? argv[at + 1] : fallback;
}

// ── backend ────────────────────────────────────────────────────────────────

/**
 * The router's rows, each carrying the URL the server actually answers on.
 *
 * `parseRouteTable` gives the access level, `parseMountedHandlers` gives the
 * nest prefix, and the two walk the same registrations in the same order — so
 * they are joined on source line rather than re-derived here.
 */
function backendRoutes() {
  const table = loadRouteTable(ROUTER);
  const prefixByLine = new Map();
  for (const row of loadMountedHandlers(ROUTER)) prefixByLine.set(row.line, row.prefix);

  return table.map((row) => {
    const prefix = prefixByLine.get(row.line) ?? null;
    // The trailing slash is part of the path, not decoration. Six registrations
    // are spelled with one on purpose — `/v2/`, `/v2/{owner}/{repo}/blobs/
    // uploads/`, `/api-docs/` and the two pypi ones — because that is what
    // docker, pip and a browser actually send, and each is mounted beside its
    // slashless twin. Folding them together made the artefact carry six
    // duplicate rows and, worse, made every test that spells the real client
    // URL invisible: `.../blobs/uploads/"` fails the route boundary of
    // `.../blobs/uploads`, so the row that a `docker push` test drives ten
    // times over reads as untested.
    const url = prefix === null ? null : `${prefix}${row.path}`.replace(/\/{2,}$/, '/') || '/';
    return { ...row, prefix, url, access: shortAccess(row.access) };
  });
}

/** `Foreign(Handler { module: "oci.rs", … })` → `Foreign:oci.rs`. */
function shortAccess(access) {
  if (!access.startsWith('Foreign')) return access;
  const module = access.match(/module:\s*"([^"]+)"/);
  const layer = access.match(/layer:\s*([A-Z_]+)/);
  if (module) return `Foreign:${module[1]}`;
  if (layer) return `Foreign:${layer[1]}`;
  return 'Foreign';
}

/** Route path → matcher. `{id}` matches one segment, whatever it is named. */
function pathMatches(routeUrl, callUrl) {
  const left = routeUrl.split('/');
  const right = callUrl.split('/');
  if (left.length !== right.length) return false;
  return left.every((segment, i) => {
    const other = right[i];
    if (segment.startsWith('{') && other.startsWith('{')) return true;
    // A wildcard tail (`{*path}`) swallows whatever the client spells there.
    if (segment.startsWith('{*')) return true;
    return segment === other;
  });
}

// ── coverage ───────────────────────────────────────────────────────────────

/**
 * Whether any test source mentions a URL this route would serve.
 *
 * Deliberately weak, and labelled as such in the report: a textual hit proves a
 * test *names* the route, not that it asserts anything useful about it. It is
 * still the difference that matters here — a route no test file so much as
 * spells is certainly untested, and that is the list we are after.
 */
function coverageIndex() {
  const corpora = {
    rust: collectFiles('crates', (f) => f.includes(`${path.sep}tests${path.sep}`) && f.endsWith('.rs')),
    web: collectFiles('web/src', (f) => f.endsWith('.test.ts')),
    // The oracle's synthetic routes test this scanner; feeding that source back
    // into its own production result would make the proof self-fulfilling.
    // Other script gates remain evidence under the deliberately weak corpus-hit
    // definition below.
    smoke: collectFiles('scripts', (f) => (
      (f.endsWith('.mjs') || f.endsWith('.sh'))
      && path.basename(f) !== 'ui-inventory-oracle-contract-check.mjs'
    )),
  };
  const sources = {};
  for (const [name, files] of Object.entries(corpora)) {
    sources[name] = files.map((file) => ({
      file,
      source: testSourceView(file, readFileSync(file, 'utf8')),
    }));
  }
  return sources;
}

/** Comment-free, string-bearing source: only executable test text may count. */
export function testSourceView(file, source) {
  if (file.endsWith('.rs')) return stripRustComments(source);
  if (file.endsWith('.mjs') || file.endsWith('.js') || file.endsWith('.ts')) {
    return productionTsSource(source);
  }
  if (file.endsWith('.sh')) {
    return source.split('\n').map((line) => {
      let quote = '';
      let escaped = false;
      for (let i = 0; i < line.length; i += 1) {
        const ch = line[i];
        if (escaped) {
          escaped = false;
          continue;
        }
        if (ch === '\\' && quote !== "'") {
          escaped = true;
          continue;
        }
        if (quote) {
          if (ch === quote) quote = '';
          continue;
        }
        if (ch === '"' || ch === "'") {
          quote = ch;
          continue;
        }
        if (ch === '#' && (i === 0 || /\s/.test(line[i - 1]))) {
          return `${line.slice(0, i)}${' '.repeat(line.length - i)}`;
        }
      }
      return line;
    }).join('\n');
  }
  return source;
}

function routePattern(url) {
  const pattern = url
    .split('/')
    .map((segment) => (segment.startsWith('{') ? '[^/"\'`\\s?]+' : segment.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
    .join('/');
  // A route is a complete path, not a prefix. Without the boundary, the test
  // for `/statuses` also colours `/status` under the same method.
  return new RegExp(`${pattern}(?=$|[?"'\\x60\\s),;\\]}])`, 'g');
}

const HTTP_METHODS = 'GET|HEAD|POST|PUT|PATCH|DELETE|OPTIONS';

function evidenceWindow(source, at, length) {
  const beforeSemicolon = source.lastIndexOf(';', at);
  const beforeParagraph = source.lastIndexOf('\n\n', at);
  const start = Math.max(beforeSemicolon + 1, beforeParagraph + 2, at - 800);
  const afterSemicolon = source.indexOf(';', at + length);
  const afterParagraph = source.indexOf('\n\n', at + length);
  const candidates = [afterSemicolon, afterParagraph].filter((value) => value !== -1);
  const boundary = candidates.length ? Math.min(...candidates) + 1 : source.length;
  const end = Math.min(boundary, at + length + 800);
  return { text: source.slice(start, end), routeAt: at - start };
}

function explicitMethods(window) {
  const found = [];
  const patterns = [
    new RegExp(`\\.(${HTTP_METHODS.toLowerCase()})\\s*\\(`, 'gi'),
    new RegExp(`\\bmethod\\s*[:=(]\\s*(?:[A-Za-z_$][\\w$]*::)*["']?(${HTTP_METHODS})["']?`, 'gi'),
    new RegExp(`\\b(?:[A-Za-z_$][\\w$]*::)*Method::(${HTTP_METHODS})\\b`, 'gi'),
    new RegExp(`(?:^|\\s)(?:-X|--request)\\s+["']?(${HTTP_METHODS})["']?`, 'gi'),
    new RegExp(`\\b(?:request|jsonRequest|apiRequest|fetch)\\s*(?:<[^>]*>)?\\(\\s*["'](${HTTP_METHODS})["']\\s*,`, 'gi'),
  ];
  for (const re of patterns) {
    let match;
    while ((match = re.exec(window.text)) !== null) {
      found.push({ method: match[1].toUpperCase(), at: match.index });
    }
  }
  return found;
}

export function sourceTouchesRoute(source, method, url) {
  if (!url) return false;
  const re = routePattern(url);
  let match;
  while ((match = re.exec(source)) !== null) {
    const window = evidenceWindow(source, match.index, match[0].length);
    const methods = explicitMethods(window);
    if (methods.length === 0) {
      // `request(url)` / `fetch(url)` and the corresponding call assertion are
      // GET by convention. A non-GET route needs an explicit verb.
      if (method === 'GET') return true;
      continue;
    }
    methods.sort((left, right) => (
      Math.abs(left.at - window.routeAt) - Math.abs(right.at - window.routeAt)
    ));
    if (methods[0].method === method) return true;
  }
  return false;
}

export function touchedBy(corpora, method, url) {
  if (!url) return [];
  return Object.entries(corpora)
    .filter(([suite, files]) => {
      // Browser client modules call `request('/repos/…')`; `_base.svelte`
      // prepends `/api/v1` at runtime. Tests correctly assert that client-side
      // spelling, so compare both forms for the web corpus rather than forcing
      // it to copy the transport prefix into prose.
      const urls = suite === 'web' && url.startsWith(API_BASE)
        ? [url, url.slice(API_BASE.length) || '/']
        : [url];
      return files.some(({ source }) => urls.some((candidate) => sourceTouchesRoute(source, method, candidate)));
    })
    .map(([name]) => name);
}

// ── frontend ───────────────────────────────────────────────────────────────

function apiSurface() {
  const byMember = new Map();
  for (const file of collectFiles(API_DIR, (f) => f.endsWith('.ts') && !f.includes('.test.'))) {
    for (const row of parseApiSurface(readFileSync(file, 'utf8'), file)) {
      if (!byMember.has(row.symbol)) byMember.set(row.symbol, []);
      byMember.get(row.symbol).push(row);
    }
  }
  return byMember;
}

/** Page route id: `web/src/routes/[owner]/[repo]/issues/+page.svelte` → `/[owner]/[repo]/issues`. */
function routeIdOf(file) {
  const rel = file.slice(`${ROUTES_DIR}/`.length).replace(/\/?\+page\.svelte$/, '');
  return `/${rel}`.replace(/\/+$/, '') || '/';
}

/**
 * A page's controls, including the ones its shared components own.
 *
 * A delegated control is reported against the page, because that is where a
 * person meets it — with `via` naming the component so the same button mounted
 * on four pages is still one place to fix.
 */
function mergeMounts(pageInv, components) {
  const merged = pageInv.elements.map((element) => ({ ...element, via: null }));

  for (const mount of pageInv.mounts) {
    const component = components.get(mount.name);
    if (!component) continue;
    for (const element of component.elements) {
      // A control wired to a callback prop reaches whatever the *page* bound to
      // that prop — that binding is the only place the real call is visible.
      const bound = element.reachesProps
        .filter((prop) => mount.props[prop])
        .flatMap((prop) => {
          const expr = mount.props[prop];
          const names = expr.match(/[A-Za-z_$][\w$]*/g) || [];
          return names.filter((name) => pageInv.declarations.includes(name));
        });
      const viaPage = [...new Set(bound.flatMap((name) => {
        const owner = pageInv.elements.find((e) => e.handlerNames.includes(name));
        return owner ? owner.reaches : [];
      }))];
      merged.push({
        ...element,
        via: mount.name,
        boundHandlers: bound,
        reaches: [...new Set([...element.reaches, ...viaPage])],
      });
    }
  }
  return merged;
}

// ── build ──────────────────────────────────────────────────────────────────

export function buildInventory() {
  const routes = backendRoutes();
  const surface = apiSurface();
  const coverage = coverageIndex();

  const components = new Map();
  for (const file of collectFiles(COMPONENT_DIR, (f) => f.endsWith('.svelte'))) {
    components.set(path.basename(file, '.svelte'), parsePageInventory(readFileSync(file, 'utf8'), file));
  }

  const resolveSymbol = (symbol) => {
    const rows = surface.get(symbol) || [];
    return rows.map((row) => {
      const url = `${API_BASE}${row.path}`;
      const opaque = row.path.includes(OPAQUE_SEGMENT);
      const route = opaque ? null : routes.find((r) => r.method === row.method && r.url && pathMatches(r.url, url));
      return {
        symbol,
        method: row.method,
        url,
        opaque,
        // The URL as the *router* spells it. The client's own spelling names
        // parameters differently (`{id}` where the server says `{labelId}`), so
        // comparing call URLs to route URLs as strings silently finds nothing —
        // which reads as "the UI reaches almost none of the API".
        routeUrl: route ? route.url : null,
        access: route ? route.access : null,
        handler: route ? route.handler : null,
        matched: Boolean(route),
        testedIn: route ? touchedBy(coverage, route.method, route.url) : [],
      };
    });
  };

  const pages = [];
  for (const file of collectFiles(ROUTES_DIR, (f) => f.endsWith('+page.svelte'))) {
    const inv = parsePageInventory(readFileSync(file, 'utf8'), file);
    const elements = mergeMounts(inv, components);
    pages.push({
      route: routeIdOf(file),
      file,
      admin: routeIdOf(file).startsWith('/admin'),
      controls: elements.map((element) => ({
        tag: element.tag,
        label: element.label,
        line: element.line,
        via: element.via,
        href: element.href,
        handler: element.handler,
        calls: element.reaches.flatMap(resolveSymbol),
        unresolved: element.reaches.length === 0 && Boolean(element.handler),
      })),
      passive: inv.passiveCalls.flatMap(resolveSymbol),
    });
  }

  // Which declared routes nothing in the UI reaches. Not a defect by itself —
  // git, OCI and the CI runner are legitimate non-browser clients — but it is
  // the list that says which of those a browser test can never cover.
  const reachedUrls = new Set(
    pages.flatMap((p) => [...p.controls.flatMap((c) => c.calls), ...p.passive])
      .filter((c) => c.matched)
      .map((c) => `${c.method} ${c.routeUrl}`),
  );

  const inventory = {
    generatedFrom: { router: ROUTER, apiDir: API_DIR, routesDir: ROUTES_DIR, componentDir: COMPONENT_DIR },
    // Deliberately no corpus file counts here. They are diagnostics, not
    // inventory, and embedding them made the artefact change whenever any
    // unrelated script was added to `scripts/` — a ratchet that goes red for
    // reasons its own subject knows nothing about is a ratchet people disable.
    routes: routes.map((r) => ({
      method: r.method,
      url: r.url,
      access: r.access,
      handler: r.handler,
      testedIn: touchedBy(coverage, r.method, r.url),
      reachedFromUi: r.url ? reachedUrls.has(`${r.method} ${r.url}`) : false,
    })),
    pages,
  };
  applyUiAccessSweepCoverage(inventory, loadUiAccessSweepSpec(ROOT));
  return inventory;
}


// ── report ─────────────────────────────────────────────────────────────────

const pct = (n, total) => (total ? `${Math.round((n / total) * 100)}%` : '—');
const esc = (value) => String(value ?? '').replace(/\|/g, '\\|');

/** A control's most useful name: its label, else its href, else its handler. */
function nameOf(control) {
  if (control.label) return control.label;
  if (control.href) return `→ ${control.href}`;
  if (control.handler) return `${control.handler.slice(0, 40)}`;
  return `<${control.tag}>`;
}

export function renderMarkdown(inv) {
  const routes = inv.routes;
  const controls = inv.pages.flatMap((p) => p.controls);
  const uiRoutes = routes.filter((r) => r.reachedFromUi);
  const frontendTested = (r) => ['web', 'smoke', 'browser'].some((suite) => r.testedIn.includes(suite));
  const out = [];

  out.push('# ForgeKeep — UI Inventory (generated)');
  out.push('');
  out.push('> **Сгенерировано.** Не править руками — перегенерировать:');
  out.push('> `node scripts/ui-inventory.mjs`');
  out.push('>');
  out.push('> Выводится из исходников: роутер (`crates/rg-http/src/routes.rs`), клиент');
  out.push('> (`web/src/lib/api`), страницы (`web/src/routes`) и общие компоненты');
  out.push('> (`web/src/lib/components`). Ручной близнец — `docs/FEATURE_INVENTORY.md`.');
  out.push('>');
  out.push('> **Что значит «покрыт».** `rust` / `web` / `smoke` пока отвечают на слабый');
  out.push('> вопрос — *называет ли исполняемый тестовый код метод и полный URL роута*.');
  out.push('> Комментарии и тот же URL под другим HTTP-методом coverage не создают. `browser` сильнее:');
  out.push('> manifest называет ровно один живой control/passive call, а runtime проводит');
  out.push('> его через owner + outsider и сверяет фактический статус с `Access`.');
  out.push('> Текстовое упоминание само по себе всё ещё НЕ означает полезного теста.');
  out.push('');

  out.push('## Сводка');
  out.push('');
  out.push('| | |');
  out.push('|---|---|');
  out.push(`| Роутов в роутере (с объявленным \`Access\`) | ${routes.length} |`);
  out.push(`| Из них достижимы из браузера | ${uiRoutes.length} (${pct(uiRoutes.length, routes.length)}) |`);
  out.push(`| Страниц | ${inv.pages.length} |`);
  out.push(`| Интерактивных элементов | ${controls.length} |`);
  out.push(`| — из них дёргают API | ${controls.filter((c) => c.calls.length).length} |`);
  out.push(`| — приходят из общих компонентов | ${controls.filter((c) => c.via).length} |`);
  out.push(`| Browser sweep: сценариев / записей инвентаря / роутов | ${inv.browserSweep.scenarios} / ${inv.browserSweep.coveredEntries} / ${inv.browserSweep.coveredRoutes} |`);
  out.push(`| **UI-роутов без единого web/smoke/browser-теста** | **${uiRoutes.filter((r) => !frontendTested(r)).length}** |`);
  out.push(`| UI-роутов без corpus-hit и browser-сценария | ${uiRoutes.filter((r) => !r.testedIn.length).length} |`);
  out.push('');

  out.push('## По уровню доступа');
  out.push('');
  out.push('| `Access` | роутов | достижимы из UI | нет фронт-теста | нет corpus/browser coverage |');
  out.push('|---|---:|---:|---:|---:|');
  const byAccess = new Map();
  for (const route of routes) {
    if (!byAccess.has(route.access)) byAccess.set(route.access, []);
    byAccess.get(route.access).push(route);
  }
  for (const [access, rows] of [...byAccess].sort((a, b) => b[1].length - a[1].length)) {
    const ui = rows.filter((r) => r.reachedFromUi);
    out.push(`| \`${esc(access)}\` | ${rows.length} | ${ui.length} | ${ui.filter((r) => !frontendTested(r)).length} | ${rows.filter((r) => !r.testedIn.length).length} |`);
  }
  out.push('');

  out.push('## Страницы');
  out.push('');
  out.push('| Страница | элементов | дёргают API | из компонентов |');
  out.push('|---|---:|---:|---:|');
  for (const page of [...inv.pages].sort((a, b) => b.controls.length - a.controls.length)) {
    const flag = page.admin ? ' 🔒' : '';
    out.push(`| \`${esc(page.route)}\`${flag} | ${page.controls.length} | ${page.controls.filter((c) => c.calls.length).length} | ${page.controls.filter((c) => c.via).length} |`);
  }
  out.push('');

  out.push('## План тестов: элемент → роут → уровень доступа');
  out.push('');
  out.push('Каждая строка — один сценарий e2e. `Access` говорит, какая персона обязана');
  out.push('пройти и какая обязана получить отказ.');
  out.push('');
  for (const page of inv.pages) {
    const acting = page.controls.filter((c) => c.calls.length);
    if (!acting.length && !page.passive.length) continue;
    out.push(`### \`${page.route}\`${page.admin ? ' 🔒' : ''}`);
    out.push('');
    out.push('| Элемент | Откуда | Вызов | `Access` | тест |');
    out.push('|---|---|---|---|---|');
    for (const control of acting) {
      for (const call of control.calls) {
        const where = control.via ? `\`${control.via}\`` : `:${control.line}`;
        const tested = call.testedIn.length ? call.testedIn.join('+') : '**—**';
        out.push(`| ${esc(nameOf(control))} | ${where} | \`${call.method} ${esc(call.routeUrl || call.url)}\` | \`${esc(call.access || '?')}\` | ${tested} |`);
      }
    }
    for (const call of page.passive) {
      const tested = call.testedIn.length ? call.testedIn.join('+') : '**—**';
      out.push(`| _(загрузка страницы)_ | — | \`${call.method} ${esc(call.routeUrl || call.url)}\` | \`${esc(call.access || '?')}\` | ${tested} |`);
    }
    out.push('');
  }

  out.push('## Роуты, недостижимые из браузера');
  out.push('');
  out.push('Не дефект: git, OCI, LFS, CI-раннер и вебхуки — легитимные не-браузерные');
  out.push('клиенты. Это список того, что браузерный тест закрыть не может в принципе.');
  out.push('');
  out.push('| Метод | URL | `Access` | тест |');
  out.push('|---|---|---|---|');
  for (const route of routes.filter((r) => !r.reachedFromUi)) {
    out.push(`| ${route.method} | \`${esc(route.url)}\` | \`${esc(route.access)}\` | ${route.testedIn.join('+') || '**—**'} |`);
  }
  out.push('');

  return `${out.join('\n')}\n`;
}

// ── driver ─────────────────────────────────────────────────────────────────

// Guarded, so that importing this module — which `ui-inventory-contract-check`
// does to compare the sources against the committed artefacts — reads the tree
// without rewriting it. An importer that silently regenerates the files it is
// about to compare against would always agree with itself.
const invokedDirectly = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (invokedDirectly) {
  const inventory = buildInventory();
  const jsonPath = arg('json', 'docs/ui-inventory.json');
  writeFileSync(jsonPath, `${JSON.stringify(inventory, null, 2)}\n`);
  const mdPath = arg('md', 'docs/UI_INVENTORY.md');
  writeFileSync(mdPath, renderMarkdown(inventory));

  const controls = inventory.pages.flatMap((p) => p.controls);
  const ui = inventory.routes.filter((r) => r.reachedFromUi);
  const frontendTested = (r) => ['web', 'smoke', 'browser'].some((suite) => r.testedIn.includes(suite));
  console.log(`wrote ${jsonPath} and ${mdPath}`);
  console.log(`routes ${inventory.routes.length} · pages ${inventory.pages.length} · controls ${controls.length}`);
  console.log(`controls reaching an API : ${controls.filter((c) => c.calls.length).length}`);
  console.log(`routes reached from UI   : ${ui.length}`);
  console.log(`UI routes no frontend test: ${ui.filter((r) => !frontendTested(r)).length}`);
}
