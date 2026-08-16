import { beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
  request: vi.fn(),
  qs: vi.fn(() => ''),
}));

vi.mock('./_base.svelte', () => base);

import pipelinePageSource from '../../routes/[owner]/[repo]/pipelines/+page.svelte?raw';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { pipelines } from './pipelines';

function asyncFunctionSource(name: string): string {
  const start = pipelinePageSource.indexOf(`  async function ${name}(`);
  expect(start, `${name} should exist in the pipeline page`).toBeGreaterThanOrEqual(0);
  const end = pipelinePageSource.indexOf('\n  async function ', start + 1);
  return pipelinePageSource.slice(start, end === -1 ? undefined : end);
}

describe('manual pipeline trigger transport', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('loads the selected refs dispatch schema', () => {
    base.qs.mockReturnValue('?ref=refs%2Ftags%2Fv1.2.3');

    pipelines.workflowDispatchSchema('alice', 'demo', 'refs/tags/v1.2.3');

    expect(base.qs).toHaveBeenCalledWith({ ref: 'refs/tags/v1.2.3' });
    expect(base.request).toHaveBeenCalledWith(
      '/repos/alice/demo/pipelines/workflow-dispatch?ref=refs%2Ftags%2Fv1.2.3',
    );
  });

  it('sends the selected ref and user inputs on the canonical wire fields', () => {
    pipelines.trigger('alice', 'demo', 'refs/tags/v1.2.3', {
      deploy: 'true',
      target: 'production',
    });

    expect(base.request).toHaveBeenCalledWith('/repos/alice/demo/pipelines', {
      method: 'POST',
      body: JSON.stringify({
        ref: 'refs/tags/v1.2.3',
        inputs: { deploy: 'true', target: 'production' },
      }),
    });
  });
});

describe('manual pipeline trigger production wiring', () => {
  it('loads branch choices and defaults to the branch marked by Git', () => {
    const loadTriggerRefs = asyncFunctionSource('loadTriggerRefs');

    expect(loadTriggerRefs).toContain('repos.branches(owner, repo)');
    expect(loadTriggerRefs).toContain('branches.find((branch) => branch.is_default)?.name');
    expect(loadTriggerRefs).toContain("branches[0]?.name ?? ''");
    expect(loadTriggerRefs).toContain('await loadDispatchSchema(triggerRef);');
  });

  it('loads the committed schema for the selected ref and installs its defaults', () => {
    const loadDispatchSchema = asyncFunctionSource('loadDispatchSchema');
    const initialDispatchValue = pipelinePageSource.slice(
      pipelinePageSource.indexOf('  function initialDispatchValue('),
      pipelinePageSource.indexOf('\n  async function loadDispatchSchema('),
    );

    expect(loadDispatchSchema).toContain(
      'pipelines.workflowDispatchSchema(owner, repo, requestedRef)',
    );
    expect(loadDispatchSchema).toContain('const definitions = schema.inputs;');
    expect(loadDispatchSchema).toContain('definitions.map((input) => [input.name, initialDispatchValue(input)])');
    expect(pipelinePageSource).not.toContain('dispatchInputsForForm');
    expect(pipelinePageSource).not.toContain('new Map<string, WorkflowDispatchInput>');
    expect(initialDispatchValue).toContain('if (input.default !== null) return input.default;');
    expect(initialDispatchValue).toContain("if (input.type === 'boolean') return 'false';");
    expect(initialDispatchValue).toContain("if (input.type === 'choice') return input.options[0] ?? '';");
  });

  it('locks duplicate submissions until the request finishes', () => {
    const handleTrigger = asyncFunctionSource('handleTrigger');

    expect(handleTrigger).toContain('triggering ||');
    expect(handleTrigger).toContain('triggerSchemaLoading ||');
    expect(handleTrigger).toContain('triggerSchemaRef !== requestedRef');
    expect(handleTrigger).toContain('triggering = true;');
    expect(handleTrigger).toMatch(/finally\s*\{\s*triggering = false;/);

    expect(pipelinePageSource).toContain('triggerSchemaRef !== triggerRef.trim()');
    expect(pipelinePageSource).toContain('aria-busy={triggering || triggerSchemaLoading}');
  });

  it('forwards the exact ref, surfaces errors, and selects the created run', () => {
    const handleTrigger = asyncFunctionSource('handleTrigger');

    expect(handleTrigger).toContain('const requestedRef = triggerRef.trim();');
    expect(handleTrigger).toContain('pipelines.trigger(owner, repo, requestedRef, triggerInputs)');
    expect(handleTrigger).toContain('await loadPipelines();');
    expect(handleTrigger).toContain('await selectPipeline(created.id);');
    expect(handleTrigger).toContain('error = e.message;');
    expect(pipelinePageSource).toContain('value={triggerRef}');
  });

  it('renders boolean, number, string/environment, and choice controls from the schema', () => {
    expect(pipelinePageSource).toContain("input.type === 'boolean'");
    expect(pipelinePageSource).toContain("input.type === 'choice'");
    expect(pipelinePageSource).toContain("type={input.type === 'number' ? 'number' : 'text'}");
    expect(pipelinePageSource).toContain('{#each input.options as option}');
    expect(pipelinePageSource).toContain('required={input.required}');
    expect(pipelinePageSource).toContain("{input.name}{input.required ? ' *' : ''}");
  });

  it.each(['run_pipeline', 'starting', 'run_ref', 'run_ref_placeholder'])(
    'has a real label in both catalogs: pipeline.%s',
    (key) => {
      expect(en.pipeline).toHaveProperty(key);
      expect(zhCN.pipeline).toHaveProperty(key);
    },
  );
});
