import type { Theme } from './types'

/** Applies `theme` to the document root: `system` removes the override
 * attribute (letting `prefers-color-scheme` in tokens.css decide), `light`/
 * `dark` force that palette via `data-theme` regardless of the OS setting. */
export function applyTheme(theme: Theme): void {
  const root = document.documentElement
  if (theme === 'system') {
    root.removeAttribute('data-theme')
  } else {
    root.setAttribute('data-theme', theme)
  }
}
