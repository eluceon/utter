//! Global hotkeys and text injection backends for Linux (evdev preferred,
//! X11 `global-hotkey` fallback), with non-Linux stubs so the workspace
//! keeps compiling on Windows and macOS ahead of platform-specific support.

pub mod hotkey;
pub use hotkey::{
    check_permissions, create_source, parse_hotkey, HotkeyEvent, HotkeyParseError, HotkeySource,
    HotkeySpec, PermissionReport,
};

pub mod chain;
pub use chain::{injection_order, ChainInjector};

pub mod inject;
pub use inject::{ClipboardOnlyInjector, ClipboardPasteInjector, TypeInjector};

mod clipboard;
mod modifier_wait;
mod uinput_kbd;

#[cfg(target_os = "linux")]
mod hotkey_evdev;
#[cfg(target_os = "linux")]
mod hotkey_x11;
