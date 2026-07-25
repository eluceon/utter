import { mount } from 'svelte'
import { getCurrentWindow } from '@tauri-apps/api/window'
import './app.css'
import App from './App.svelte'
import Hud from './hud/Hud.svelte'

const target = document.getElementById('app')!

// The HUD is a separate, minimal Tauri window (see `tauri.conf.json`'s
// "hud" window and `src-tauri/src/sink.rs`, which shows/hides it as the
// dictation phase changes); every other window label gets the regular
// settings-shell `App`.
const app =
  getCurrentWindow().label === 'hud' ? mount(Hud, { target }) : mount(App, { target })

export default app
