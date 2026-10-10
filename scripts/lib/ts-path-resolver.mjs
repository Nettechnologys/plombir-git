import { productionTsCode, productionTsSource } from './ts-source.mjs';

const MAX_VARIANTS = 128;

function readBalanced(code, start, open, close) {
  if (code[start] !== open) return null;
  let depth = 0;
  for (let i = start; i < code.length; i += 1) {
    if (code[i] === open) depth += 1;
    else if (code[i] === close) {
      depth -= 1;
      if (depth === 0) return { start, end: i };
    }
  }
  return null;
}

function splitTopLevel(code, start, end) {
  const parts = [];
  let depth = 0;
  let partStart = start;
  for (let i = start; i < end; i += 1) {
    const ch = code[i];
    if (ch === '(' || ch === '[' || ch === '{' || ch === '<') depth += 1;
    else if (ch === ')' || ch === ']' || ch === '}' || ch === '>') depth -= 1;
    else if (ch === ',' && depth === 0) {
      parts.push({ start: partStart, end: i });
      partStart = i + 1;
    }
  }
  if (partStart < end) parts.push({ start: partStart, end });
  return parts;
}

function stringLiteralValue(value) {
  const text = String(value || '').trim();
  if (text.length < 2 || !['\'', '"', '`'].includes(text[0]) || text.at(-1) !== text[0]) {
    return null;
  }
  const body = text.slice(1, -1);
  if (text[0] === '`' && body.includes('${')) return null;
  return body.replace(/\\([\\'"`])/g, '$1');
}

function stringUnions(text) {
  const unions = new Map();
  const alias = /\btype\s+([A-Za-z_$][\w$]*)\s*=\s*([^;]+);/g;
  let match;
  while ((match = alias.exec(text)) !== null) {
    const members = match[2].split('|').map(stringLiteralValue);
    if (members.length > 0 && members.every((member) => member !== null)) {
      unions.set(match[1], members);
    }
  }
  return unions;
}

function pathReturnBody(text) {
  const returned = text.match(
    /return\s+([`'"])((?:\/|\$\{repoPath\([^{}]*\)\})(?:[^\\`'"\n]|\\.|\$\{[^{}]*\})*)\1\s*;?\s*$/,
  );
  if (!returned) return null;

  const bindings = new Map();
  let prefix = text.slice(0, returned.index);
  while (prefix.trim()) {
    const declaration = prefix.match(
      /^\s*const\s+([A-Za-z_$][\w$]*)(?:\s*:[^=;]+)?\s*=\s*([^;]+);/,
    );
    if (!declaration || bindings.has(declaration[1])) return null;
    bindings.set(declaration[1], declaration[2].trim());
    prefix = prefix.slice(declaration[0].length);
  }
  return { template: returned[2], bindings };
}

function returnedPathHelpers(code, text, { exportedOnly = false, helperNames = null } = {}) {
  const helpers = new Map();
  const header = /(?:^|[^A-Za-z0-9_$])(export\s+)?(?:async\s+)?function\s+([A-Za-z_$][\w$]*)\s*\(/g;
  let match;
  while ((match = header.exec(code)) !== null) {
    const exported = Boolean(match[1]);
    const name = match[2];
    if ((exportedOnly && !exported) || (helperNames && !helperNames.has(name))) continue;
    const open = header.lastIndex - 1;
    const params = readBalanced(code, open, '(', ')');
    if (!params) continue;
    const braceAt = code.indexOf('{', params.end + 1);
    const body = braceAt === -1 ? null : readBalanced(code, braceAt, '{', '}');
    if (!body) continue;

    const returned = pathReturnBody(text.slice(body.start + 1, body.end));
    if (!returned) continue;

    const parsedParams = splitTopLevel(code, params.start + 1, params.end)
      .map((part) => text.slice(part.start, part.end).trim())
      .map((raw) => {
        const parsed = raw.match(/^(?:\.\.\.)?([A-Za-z_$][\w$]*)(?:\s*\??\s*:\s*([A-Za-z_$][\w$]*))?/);
        return parsed ? { name: parsed[1], type: parsed[2] ?? null } : null;
      })
      .filter(Boolean);
    helpers.set(name, {
      params: parsedParams,
      template: returned.template,
      bindings: returned.bindings,
    });
  }
  return helpers;
}

export function createLocalPathResolver(source, options = {}) {
  const text = productionTsSource(source);
  const code = productionTsCode(source);
  return {
    helpers: returnedPathHelpers(code, text, options),
    unions: stringUnions(text),
  };
}

export function multiSegmentUnionMembers(resolver) {
  return [...resolver.unions.values()]
    .flat()
    .filter((member) => member.includes('/'))
    .map((member) => member.split('/'));
}

function mergeConstraints(left, right) {
  const merged = { ...left };
  for (const [key, value] of Object.entries(right)) {
    if (Object.hasOwn(merged, key) && merged[key] !== value) return null;
    merged[key] = value;
  }
  return merged;
}

function escapeRegex(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function objectLiteralBindings(source) {
  const text = String(source || '').trim();
  if (!text.startsWith('{') || !text.endsWith('}')) return null;
  const code = productionTsCode(text);
  const bindings = new Map();
  for (const part of splitTopLevel(code, 1, code.length - 1)) {
    const raw = text.slice(part.start, part.end).trim();
    const shorthand = raw.match(/^([A-Za-z_$][\w$]*)$/);
    if (shorthand) {
      bindings.set(shorthand[1], shorthand[1]);
      continue;
    }
    const explicit = raw.match(/^([A-Za-z_$][\w$]*)\s*:\s*([\s\S]+)$/);
    if (!explicit || !explicit[2].trim()) return null;
    bindings.set(explicit[1], explicit[2].trim());
  }
  return bindings;
}

function expandHelperBinding(expression, bindings) {
  let current = String(expression || '').trim();
  const seen = new Set();
  while (bindings.has(current)) {
    if (seen.has(current)) return 'unresolvedPathExpression()';
    seen.add(current);
    current = bindings.get(current).trim();
  }
  return current;
}

function rewriteTemplateExpressions(template, rewrite) {
  return template.replace(/\$\{([^{}]*)\}/g, (_whole, expression) => {
    const replacement = rewrite(expression);
    if (replacement && typeof replacement === 'object' && 'literal' in replacement) {
      return replacement.literal;
    }
    return `\${${replacement}}`;
  });
}

function substituteDynamicArgument(expression, param, argument) {
  let rewritten = expression;
  const object = objectLiteralBindings(argument);
  const property = new RegExp(`\\b${escapeRegex(param.name)}\\.([A-Za-z_$][\\w$]*)\\b`, 'g');
  rewritten = rewritten.replace(property, (_whole, name) => (
    object?.get(name) ?? 'unresolvedPathExpression()'
  ));

  const bare = new RegExp(`\\b${escapeRegex(param.name)}\\b`, 'g');
  if (!bare.test(rewritten)) return rewritten;
  if (/^[A-Za-z_$][\w$]*$/.test(argument.trim())) {
    return rewritten.replace(bare, argument.trim());
  }
  return rewritten.replace(bare, 'unresolvedPathExpression()');
}

function resolveHelperCall(helper, args, unions) {
  if (args.length !== helper.params.length) return [];
  let variants = [{
    value: rewriteTemplateExpressions(
      helper.template,
      (expression) => expandHelperBinding(expression, helper.bindings),
    ),
    constraints: {},
  }];

  for (let index = 0; index < helper.params.length; index += 1) {
    const param = helper.params[index];
    const arg = args[index].trim();
    const literal = stringLiteralValue(arg);
    const identifier = arg.match(/^([A-Za-z_$][\w$]*)$/)?.[1] ?? null;
    const union = param.type ? unions.get(param.type) : null;
    let choices;
    if (literal !== null) {
      choices = [{ staticValue: literal, constraints: {} }];
    } else if (union && identifier) {
      choices = union.map((member) => ({
        staticValue: member,
        constraints: { [identifier]: member },
      }));
    } else {
      choices = [{ staticValue: null, constraints: {} }];
    }

    variants = variants.flatMap((variant) => choices.map((choice) => ({
      value: rewriteTemplateExpressions(variant.value, (expression) => {
        const trimmed = expression.trim();
        if (choice.staticValue !== null && trimmed === param.name) {
          return { literal: choice.staticValue };
        }
        return substituteDynamicArgument(trimmed, param, arg);
      }),
      constraints: { ...variant.constraints, ...choice.constraints },
    })));
    if (variants.length > MAX_VARIANTS) {
      throw new Error(`local path resolver exceeded ${MAX_VARIANTS} variants`);
    }
  }
  return variants;
}

function previousWord(code, at) {
  let end = at;
  while (end > 0 && /\s/.test(code[end - 1])) end -= 1;
  let start = end;
  while (start > 0 && /[A-Za-z0-9_$]/.test(code[start - 1])) start -= 1;
  return code.slice(start, end);
}

function firstResolvableCall(source, resolver) {
  const code = productionTsCode(source);
  for (const [name, helper] of resolver.helpers) {
    let cursor = 0;
    while (true) {
      const at = code.indexOf(name, cursor);
      if (at === -1) break;
      const before = code[at - 1];
      const after = code[at + name.length];
      if ((before && /[A-Za-z0-9_$]/.test(before)) || before === '.'
        || (after && /[A-Za-z0-9_$]/.test(after)) || previousWord(code, at) === 'function') {
        cursor = at + 1;
        continue;
      }
      let open = at + name.length;
      while (/\s/.test(code[open] || '')) open += 1;
      if (code[open] !== '(') {
        cursor = at + 1;
        continue;
      }
      const invocation = readBalanced(code, open, '(', ')');
      if (!invocation) {
        cursor = at + 1;
        continue;
      }
      const args = splitTopLevel(code, open + 1, invocation.end)
        .map((part) => source.slice(part.start, part.end));
      const variants = resolveHelperCall(helper, args, resolver.unions);
      if (variants.length === 0) {
        cursor = invocation.end + 1;
        continue;
      }

      let start = at;
      while (start > 0 && /\s/.test(source[start - 1])) start -= 1;
      let end = invocation.end + 1;
      while (/\s/.test(source[end] || '')) end += 1;
      const nestedTemplate = source[start - 1] === '{' && source[start - 2] === '$'
        && source[end] === '}';
      if (nestedTemplate) {
        start -= 2;
        end += 1;
      }
      return { start, end, nestedTemplate, variants };
    }
  }
  return null;
}

/**
 * Expand one-hop local URL builders into concrete source variants.
 *
 * Each result keeps the literal-union choice that produced it. Callers use
 * those constraints to bind a shared component's `target="…"` prop to only
 * the route family mounted on that page.
 */
export function expandLocalPathCalls(source, resolver) {
  const pending = [{ source, constraints: {} }];
  const complete = [];
  while (pending.length > 0) {
    const current = pending.pop();
    const call = firstResolvableCall(current.source, resolver);
    if (!call) {
      complete.push(current);
      continue;
    }
    for (const variant of call.variants) {
      const constraints = mergeConstraints(current.constraints, variant.constraints);
      if (!constraints) continue;
      const replacement = call.nestedTemplate ? variant.value : `\`${variant.value}\``;
      pending.push({
        source: `${current.source.slice(0, call.start)}${replacement}${current.source.slice(call.end)}`,
        constraints,
      });
    }
    if (pending.length + complete.length > MAX_VARIANTS) {
      throw new Error(`local path resolver exceeded ${MAX_VARIANTS} source variants`);
    }
  }

  const unique = new Map();
  for (const row of complete) {
    const constraints = Object.fromEntries(Object.entries(row.constraints).sort());
    unique.set(`${row.source}\u0000${JSON.stringify(constraints)}`, { ...row, constraints });
  }
  return [...unique.values()];
}
