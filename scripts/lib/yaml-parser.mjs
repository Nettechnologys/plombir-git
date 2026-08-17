import { spawnSync } from 'node:child_process';

// Both programs answer the same way: exit 0 with the document as JSON on
// stdout, exit 2 with the parser's own diagnostic on stderr when YAML is
// invalid. Keeping the probes here gives every repository YAML gate the same
// fail-loud fallback instead of letting each one grow a subtly different parser.
const PY_PROGRAM = `
import json, sys, yaml
try:
    document = yaml.safe_load(open(sys.argv[1], encoding="utf-8"))
except yaml.YAMLError as error:
    print(str(error), file=sys.stderr)
    raise SystemExit(2)
json.dump(document, sys.stdout, default=str)
`;

const RB_PROGRAM = `
require "yaml"
require "json"
require "date"
begin
  document = YAML.safe_load(File.read(ARGV[0]), aliases: true, permitted_classes: [Date, Time])
rescue Psych::SyntaxError => error
  warn error.message
  exit 2
end
print JSON.generate(document)
`;

const PARSERS = [
  { name: 'python3 + PyYAML', tool: 'python3', probe: ['-c', 'import yaml'], args: (file) => ['-c', PY_PROGRAM, file] },
  { name: 'ruby + psych', tool: 'ruby', probe: ['-ryaml', '-e', ''], args: (file) => ['-e', RB_PROGRAM, file] },
];

export function selectYamlParser() {
  const missing = [];
  for (const parser of PARSERS) {
    const probe = spawnSync(parser.tool, parser.probe, { encoding: 'utf8' });
    if (!probe.error && probe.status === 0) return { parser, missing };
    missing.push(parser.name);
  }
  return { parser: null, missing };
}

export function parseYamlFile(parser, file) {
  const result = spawnSync(parser.tool, parser.args(file), { encoding: 'utf8' });
  const diagnostic = (result.stderr ?? '').trim();

  if (result.error) {
    return { ok: false, kind: 'spawn', message: result.error.message };
  }
  if (result.status === 2) {
    return { ok: false, kind: 'syntax', diagnostic };
  }
  if (result.status !== 0) {
    return { ok: false, kind: 'parser', status: result.status, diagnostic };
  }

  try {
    return { ok: true, document: JSON.parse(result.stdout) };
  } catch (error) {
    return { ok: false, kind: 'output', message: error.message };
  }
}
