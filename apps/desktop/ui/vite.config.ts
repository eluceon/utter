/// <reference types="vitest/config" />
import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// Tauri expects a fixed, predictable dev server: https://v2.tauri.app/start/frontend/vite/
const host = process.env.TAURI_DEV_HOST

// https://vite.dev/config/
export default defineConfig({
  plugins: [svelte()],

  // Prevent Vite from clobbering Rust compiler messages in `cargo tauri dev`.
  clearScreen: false,
  server: {
    host: host || false,
    port: 1420,
    strictPort: true,
    watch: {
      // Don't watch the Rust side; the Cargo build handles its own reloads.
      ignored: ['**/src-tauri/**'],
    },
  },

  // Store/type logic under test is plain TS with no DOM dependency, so the
  // default `node` environment is enough — no jsdom/happy-dom needed.
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
})
