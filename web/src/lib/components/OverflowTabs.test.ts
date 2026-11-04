import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import OverflowTabsHarness from '$lib/test/OverflowTabsHarness.svelte';
import { button, click, element, renderComponent, settle, type RenderedComponent } from '$lib/test/render';

let rendered: RenderedComponent | undefined;
let available = 360;
let fontScale = 1;
let nextFrame = 0;
const frames = new Map<number, FrameRequestCallback>();
const observers = new Set<ResizeObserverCallback>();
let fontEvents: EventTarget;

async function flushLayout() {
  await settle();
  for (let pass = 0; frames.size && pass < 10; pass++) {
    const pending = [...frames.values()];
    frames.clear();
    pending.forEach(callback => callback(0));
    await settle();
  }
  expect(frames.size).toBe(0);
}

async function resize(width: number) {
  available = width;
  for (const callback of observers) callback([], {} as ResizeObserver);
  await flushLayout();
}

function visibleLinks() {
  return Array.from(rendered!.container.querySelectorAll<HTMLAnchorElement>('nav > a'));
}

function trigger() {
  return element<HTMLButtonElement>(rendered!.container, '.overflow-trigger');
}

async function key(target: Element, value: string) {
  target.dispatchEvent(new KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true }));
  await flushLayout();
}

beforeEach(() => {
  available = 360;
  fontScale = 1;
  frames.clear();
  observers.clear();
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { frames.set(++nextFrame, callback); return nextFrame; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => frames.delete(id));
  vi.stubGlobal('ResizeObserver', class {
    callback: ResizeObserverCallback;
    constructor(callback: ResizeObserverCallback) { this.callback = callback; observers.add(callback); }
    observe() {}
    disconnect() { observers.delete(this.callback); }
  });
  fontEvents = new EventTarget();
  Object.defineProperty(document, 'fonts', { configurable: true, value: Object.assign(fontEvents, { ready: Promise.resolve() }) });
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
    let width = 0;
    if (this.tagName === 'NAV') width = available;
    else if (this.hasAttribute('data-measure-tab')) {
      const text = this.textContent ?? '';
      width = (/[а-яА-Я]/.test(text) ? 140 : /[\u4e00-\u9fff]/.test(text) ? 80 : 110) * fontScale;
      if (text.includes('100000')) width += 60;
    } else if (this.hasAttribute('data-measure-more') || this.classList.contains('overflow-trigger')) width = 70;
    else if (this.getAttribute('role') === 'menu') width = 200;
    return { x: 0, y: 0, left: 0, top: 0, width, height: 40, right: width, bottom: 40, toJSON() {} };
  });
});

afterEach(async () => {
  await rendered?.destroy();
  rendered = undefined;
  expect(observers.size).toBe(0);
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  Reflect.deleteProperty(document, 'fonts');
});

describe('adaptive repository tabs', () => {
  it('redistributes individual links on resize, restores order, and leaves no hidden focus stops', async () => {
    rendered = await renderComponent(OverflowTabsHarness);
    await flushLayout();
    expect(visibleLinks().map(link => link.getAttribute('href'))).toEqual(['/code', '/issues']);
    expect(trigger().getAttribute('aria-expanded')).toBe('false');
    expect(rendered.container.querySelector('.measurement-clip a, .measurement-clip button, .measurement-clip [tabindex]')).toBeNull();
    expect(element(rendered.container, '.measurement-clip').getAttribute('aria-hidden')).toBe('true');
    expect(rendered.container.querySelector('a[href="/settings"]')).toBeNull();

    await resize(410);
    expect(visibleLinks()).toHaveLength(3);
    await resize(440);
    expect(visibleLinks().map(link => link.getAttribute('href'))).toEqual(['/code', '/issues', '/pulls', '/settings']);
    expect(rendered.container.querySelector('.overflow-trigger')).toBeNull();
    await resize(180);
    expect(visibleLinks()).toHaveLength(1);
    await resize(100);
    expect(visibleLinks()).toHaveLength(0);
    await click(trigger());
    expect(rendered.container.querySelectorAll('[role="menuitem"]')).toHaveLength(4);
  });

  it('recalculates when language, counts, fonts, and permitted tabs change without a resize', async () => {
    rendered = await renderComponent(OverflowTabsHarness);
    await flushLayout();
    await resize(440);
    await click(button(rendered.container, 'Russian'));
    await flushLayout();
    expect(visibleLinks()).toHaveLength(2);
    expect(trigger().textContent).toContain('Ещё');
    await click(button(rendered.container, 'Chinese'));
    await flushLayout();
    expect(visibleLinks()).toHaveLength(4);
    await click(button(rendered.container, 'English'));
    await click(button(rendered.container, 'Increase count'));
    await flushLayout();
    expect(visibleLinks()).toHaveLength(2);
    await click(button(rendered.container, 'Toggle settings permission'));
    await flushLayout();
    expect(visibleLinks()).toHaveLength(3);
    expect(rendered.container.querySelector('a[href="/settings"]')).toBeNull();
    fontScale = 1.5;
    fontEvents.dispatchEvent(new Event('loadingdone'));
    await flushLayout();
    expect(visibleLinks()).toHaveLength(1);
  });

  it('marks a hidden active page and supports arrows, Home/End, Escape and leaving the menu', async () => {
    rendered = await renderComponent(OverflowTabsHarness);
    await flushLayout();
    expect(trigger().classList.contains('active')).toBe(true);
    expect(trigger().getAttribute('aria-label')).toContain('Settings');
    trigger().focus();
    await key(trigger(), 'ArrowDown');
    const menuItems = Array.from(rendered.container.querySelectorAll<HTMLAnchorElement>('[role="menuitem"]'));
    expect(trigger().getAttribute('aria-expanded')).toBe('true');
    expect(document.activeElement).toBe(menuItems[0]);
    expect(menuItems[1].getAttribute('aria-current')).toBe('page');
    await key(menuItems[0], 'End');
    expect(document.activeElement).toBe(menuItems[1]);
    await key(menuItems[1], 'ArrowDown');
    expect(document.activeElement).toBe(menuItems[0]);
    await key(menuItems[0], 'Home');
    await key(menuItems[0], 'Escape');
    expect(document.activeElement).toBe(trigger());
    expect(trigger().getAttribute('aria-expanded')).toBe('false');
    await key(trigger(), 'ArrowUp');
    expect(document.activeElement?.getAttribute('href')).toBe('/settings');
    button(rendered.container, 'Outside navigation').focus();
    await flushLayout();
    expect(trigger().getAttribute('aria-expanded')).toBe('false');
    await click(button(rendered.container, 'Activate issues'));
    await flushLayout();
    expect(trigger().classList.contains('active')).toBe(false);
    expect(element(rendered.container, 'a[href="/issues"]').getAttribute('aria-current')).toBe('page');
  });

  it('keeps focus reachable when a focused tab moves to More and back into the row', async () => {
    rendered = await renderComponent(OverflowTabsHarness);
    await flushLayout();
    await resize(440);
    element<HTMLAnchorElement>(rendered.container, 'a[href="/settings"]').focus();
    await resize(360);
    expect(document.activeElement).toBe(trigger());
    await key(trigger(), 'ArrowUp');
    expect(document.activeElement?.getAttribute('href')).toBe('/settings');
    await resize(440);
    expect(document.activeElement?.getAttribute('href')).toBe('/settings');
    expect(rendered.container.querySelector('[role="menu"]')).toBeNull();
  });

  it('closes an obsolete menu when permissions remove its last item and does not reopen it later', async () => {
    rendered = await renderComponent(OverflowTabsHarness);
    await resize(410);
    await key(trigger(), 'ArrowUp');
    expect(document.activeElement?.getAttribute('href')).toBe('/settings');
    await click(button(rendered.container, 'Toggle settings permission'));
    await flushLayout();
    expect(rendered.container.querySelector('[role="menu"]')).toBeNull();
    expect(visibleLinks()).toContain(document.activeElement);
    await click(button(rendered.container, 'Toggle settings permission'));
    await flushLayout();
    expect(trigger().getAttribute('aria-expanded')).toBe('false');
  });
});
