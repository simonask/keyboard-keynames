//! The KeyLayout struct and supporting elements
#[cfg(target_os = "linux")]
#[path = "unix/mod.rs"]
mod platform;

pub use self::platform::key_layout::*;
