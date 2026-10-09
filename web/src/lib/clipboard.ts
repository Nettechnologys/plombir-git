/**
 * Copy `text` and say whether it worked (card_c30077df5603).
 *
 * `navigator.clipboard` exists only in a secure context, so on an instance
 * served over plain HTTP every copy button threw — while the button had
 * already announced "Copied", because nobody awaited the promise. This falls
 * back to the older selection-based copy and reports the outcome, so a caller
 * shows "Copied" only for a copy that happened.
 */
export async function copyToClipboard(text: string): Promise<boolean> {
  if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // Denied or unavailable: try the fallback below.
    }
  }
  if (typeof document === 'undefined') return false;
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.style.position = 'fixed';
  area.style.opacity = '0';
  document.body.append(area);
  area.select();
  try {
    return typeof document.execCommand === 'function' && document.execCommand('copy');
  } catch {
    return false;
  } finally {
    area.remove();
  }
}
