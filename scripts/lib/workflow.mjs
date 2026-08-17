import { parseYamlFile, selectYamlParser } from './yaml-parser.mjs';

// Workflow readers share this boundary so a structural assertion can never
// quietly fall back to reading YAML-shaped text. The underlying parser probe is
// kept in yaml-parser.mjs because Grafana provisioning uses the same fail-loud
// Python/Ruby fallback for non-workflow documents.
export function selectWorkflowParser() {
  return selectYamlParser();
}

export function workflowJobs(document) {
  if (!document || typeof document !== 'object' || Array.isArray(document)) return null;
  const jobs = document.jobs;
  if (!jobs || typeof jobs !== 'object' || Array.isArray(jobs)) return null;
  return jobs;
}

export function parseWorkflowFile(parser, file) {
  const result = parseYamlFile(parser, file);
  if (!result.ok) return result;
  return { ...result, jobs: workflowJobs(result.document) };
}

// Return both valid shell bodies and the structural failures a caller must
// reject before classifying a job. Ignoring a non-list `steps` or a non-string
// `run` would turn malformed workflow data into a plausible cargo-free job.
export function workflowJobRuns(definition) {
  const steps = definition?.steps;
  if (steps === undefined) return { ok: true, runs: [], invalidRuns: [] };
  if (!Array.isArray(steps)) return { ok: false, runs: [], invalidRuns: [], invalidSteps: true };

  const runs = [];
  const invalidRuns = [];
  for (const [index, step] of steps.entries()) {
    if (!step || typeof step !== 'object' || !('run' in step)) continue;
    if (typeof step.run !== 'string' || step.run.trim() === '') {
      invalidRuns.push({ index, name: step.name, value: step.run });
      continue;
    }
    runs.push(step.run);
  }
  return { ok: invalidRuns.length === 0, runs, invalidRuns };
}
