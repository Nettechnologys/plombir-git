import { afterEach, expect, it } from 'vitest';
import ThemeToggle from './components/ThemeToggle.svelte';
import { renderComponent, click, element, type RenderedComponent } from './test/render';

let rendered: RenderedComponent | undefined;
afterEach(async () => {
  await rendered?.destroy();
  rendered = undefined;
  localStorage.removeItem('plombir_git_theme');
  delete document.documentElement.dataset.theme;
  document.documentElement.style.colorScheme = '';
});

it('switches both palettes and remembers the choice across remounts', async () => {
  rendered = await renderComponent(ThemeToggle);
  await click(element(rendered.container, 'button'));
  expect(document.documentElement.dataset.theme).toBe('light');
  expect(document.documentElement.style.colorScheme).toBe('light');
  expect(localStorage.getItem('plombir_git_theme')).toBe('light');
  await rendered.destroy();
  rendered = await renderComponent(ThemeToggle);
  expect(element(rendered.container, 'button').getAttribute('aria-pressed')).toBe('true');
  await click(element(rendered.container, 'button'));
  expect(document.documentElement.dataset.theme).toBe('dark');
  expect(localStorage.getItem('plombir_git_theme')).toBe('dark');
});
