// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Works around a webkit2gtk DMA-BUF renderer bug (seen on some Mesa/NVIDIA
/// driver combinations) where the whole window content periodically goes
/// fully black. `WEBKIT_DISABLE_DMABUF_RENDERER=1` falls back to a
/// non-DMA-BUF rendering path; harmless to set even on webkit2gtk versions
/// that don't need it. Only applies on Linux (the only platform using
/// webkit2gtk), and only if the user hasn't already set it themselves.
#[cfg(target_os = "linux")]
fn apply_webkit_dmabuf_workaround() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: called at the very start of `main`, before Tauri spawns
        // any other thread or initializes the webview, so there is no
        // concurrent access to the environment from elsewhere in-process.
        unsafe {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    apply_webkit_dmabuf_workaround();

    if let Err(err) = utter_desktop_lib::run() {
        eprintln!("utter: {err}");
        std::process::exit(1);
    }
}
