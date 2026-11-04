import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import CopyableValueHarness from '../test/CopyableValueHarness.svelte';
import { button, click, element, renderComponent, settle, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent;
const writeText = vi.fn();
const originalExecCommand = document.execCommand;
function deferred() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const copyButton = () => element<HTMLButtonElement>(rendered.container, '.copy-button');
const feedback = () => element(rendered.container, '[role="status"]');

beforeEach(async () => {
  writeText.mockReset().mockResolvedValue(undefined);
  vi.stubGlobal('navigator', { clipboard: { writeText } });
  document.execCommand = vi.fn(() => false);
  rendered = await renderComponent(CopyableValueHarness);
});
afterEach(async () => {
  await rendered?.destroy();
  vi.unstubAllGlobals();
  document.execCommand = originalExecCommand;
});

describe('copyable value', () => {
  it('keeps the value manually selectable without leaking it into labels or titles', () => {
    const value = element<HTMLInputElement>(rendered.container, '.value');
    expect(value.readOnly).toBe(true);
    value.focus();
    value.select();
    expect(value.value.slice(value.selectionStart!, value.selectionEnd!)).toBe('fixture-first');
    for (const node of rendered.container.querySelectorAll('[aria-label], [title]')) {
      expect(node.getAttribute('aria-label') ?? '').not.toContain(value.value);
      expect(node.getAttribute('title') ?? '').not.toContain(value.value);
    }
  });

  it('announces success only after copying and copies again without submitting its form', async () => {
    const pending = deferred();
    writeText.mockReturnValueOnce(pending.promise);
    copyButton().focus();
    copyButton().click();
    await settle();
    expect(copyButton().getAttribute('aria-disabled')).toBe('true');
    copyButton().click();
    expect(writeText).toHaveBeenCalledOnce();
    expect(feedback().textContent?.trim()).toBe('Copying…');
    expect(copyButton().getAttribute('aria-label')).not.toBe('Copied');
    pending.resolve();
    await settle();
    expect(writeText).toHaveBeenCalledWith('fixture-first');
    expect(copyButton().getAttribute('aria-label')).toBe('Copied');
    expect(copyButton().title).toBe('Copied');
    expect(feedback().textContent?.trim()).toBe('Copied');
    expect(document.activeElement).toBe(copyButton());
    copyButton().click();
    await settle();
    expect(writeText).toHaveBeenCalledTimes(2);
    expect(element(rendered.container, 'output').textContent).toBe('0');
  });

  it('shows failure when both clipboard methods fail, and allows a successful retry', async () => {
    writeText.mockRejectedValueOnce(new Error('denied'));
    await click(copyButton());
    expect(document.execCommand).toHaveBeenCalledWith('copy');
    expect(feedback().textContent).toContain('Copying failed');
    expect(feedback().classList.contains('visually-hidden')).toBe(false);
    expect(copyButton().getAttribute('aria-label')).toBe('Copy token');
    expect(element(rendered.container, '.value').getAttribute('aria-describedby')).toBe(feedback().id);
    await click(copyButton());
    expect(feedback().textContent?.trim()).toBe('Copied');
    expect(copyButton().getAttribute('aria-label')).toBe('Copied');
  });

  it('clears old success when a later copy fails', async () => {
    await click(copyButton());
    writeText.mockRejectedValueOnce(new Error('denied'));
    await click(copyButton());
    expect(feedback().textContent).toContain('Copying failed');
    expect(copyButton().getAttribute('aria-label')).toBe('Copy token');
  });

  it('uses the existing selection fallback and restores keyboard focus', async () => {
    vi.stubGlobal('navigator', { clipboard: undefined });
    document.execCommand = vi.fn(() => {
      const fallback = document.querySelector('textarea')!;
      expect(fallback.value).toBe('fixture-first');
      fallback.focus();
      return true;
    });
    copyButton().focus();
    await click(copyButton());
    expect(feedback().textContent?.trim()).toBe('Copied');
    expect(document.activeElement).toBe(copyButton());
    expect(document.querySelector('textarea')).toBeNull();
  });

  it.each(['success', 'failure'])('ignores stale %s after replacement while the new copy is pending', async outcome => {
    const old = deferred();
    const current = deferred();
    writeText.mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise);
    await click(copyButton());
    await click(button(rendered.container, 'Replace value'));
    expect(copyButton().disabled).toBe(false);
    expect(copyButton().getAttribute('aria-label')).toBe('Copy token');
    await click(copyButton());
    if (outcome === 'success') old.resolve(); else old.reject(new Error('denied'));
    await settle();
    expect(feedback().textContent?.trim()).toBe('Copying…');
    expect(copyButton().getAttribute('aria-disabled')).toBe('true');
    expect(document.execCommand).not.toHaveBeenCalled();
    current.resolve();
    await settle();
    expect(writeText.mock.calls.map(args => args[0])).toEqual(['fixture-first', 'fixture-second']);
    expect(feedback().textContent?.trim()).toBe('Copied');
  });

  it('resets completed feedback when the value changes', async () => {
    await click(copyButton());
    await click(button(rendered.container, 'Replace value'));
    expect(feedback().textContent?.trim()).toBe('');
    expect(copyButton().getAttribute('aria-label')).toBe('Copy token');
  });

  it.each(['success', 'failure'])('does not announce or start a fallback for a pending %s after unmount', async outcome => {
    const pending = deferred();
    writeText.mockReturnValueOnce(pending.promise);
    await click(copyButton());
    const status = feedback();
    await rendered.destroy();
    if (outcome === 'success') pending.resolve(); else pending.reject(new Error('denied'));
    await settle();
    expect(document.querySelector('[role="status"]')).toBeNull();
    expect(status.textContent).not.toContain('Copied');
    expect(document.execCommand).not.toHaveBeenCalled();
    rendered = undefined!;
  });
});
