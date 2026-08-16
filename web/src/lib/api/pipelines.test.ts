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

  it('sends the selected ref on the canonical wire field', () => {
    pipelines.trigger('alice', 'demo', 'refs/tags/v1.2.3');

    expect(base.request).toHaveBeenCalledWith('/repos/alice/demo/pipelines', {
      method: 'POST',
      body: JSON.stringify({ ref: 'refs/tags/v1.2.3' }),
    });
  });
});

describe('manual pipeline trigger production wiring', () => {
  it('loads branch choices and defaults to the branch marked by Git', () => {
    const loadTriggerRefs = asyncFunctionSource('loadTriggerRefs');

    expect(loadTriggerRefs).toContain('repos.branches(owner, repo)');
    expect(loadTriggerRefs).toContain('branches.find((branch) => branch.is_default)?.name');
    expect(loadTriggerRefs).toContain("branches[0]?.name ?? ''");
  });

  it('locks duplicate submissions until the request finishes', () => {
    const handleTrigger = asyncFunctionSource('handleTrigger');

    expect(handleTrigger).toContain('if (triggering || !requestedRef) return;');
    expect(handleTrigger).toContain('triggering = true;');
    expect(handleTrigger).toMatch(/finally\s*\{\s*triggering = false;/);

    expect(pipelinePageSource).toContain('disabled={triggering || !triggerRef.trim()}');
    expect(pipelinePageSource).toContain('aria-busy={triggering}');
  });

  it('forwards the exact ref, surfaces errors, and selects the created run', () => {
    const handleTrigger = asyncFunctionSource('handleTrigger');

    expect(handleTrigger).toContain('const requestedRef = triggerRef.trim();');
    expect(handleTrigger).toContain('pipelines.trigger(owner, repo, requestedRef)');
    expect(handleTrigger).toContain('await loadPipelines();');
    expect(handleTrigger).toContain('await selectPipeline(created.id);');
    expect(handleTrigger).toContain('error = e.message;');
    expect(pipelinePageSource).toContain('bind:value={triggerRef}');
  });

  it.each(['run_pipeline', 'starting', 'run_ref', 'run_ref_placeholder'])(
    'has a real label in both catalogs: pipeline.%s',
    (key) => {
      expect(en.pipeline).toHaveProperty(key);
      expect(zhCN.pipeline).toHaveProperty(key);
    },
  );
});
