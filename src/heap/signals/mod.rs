#[cfg(unix)]
pub mod unix;
#[cfg(any(target_os="macos", target_os = "ios"))]
pub mod mach;
#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
pub use unix::install_signal_handlers;

#[cfg(windows)]
pub use windows::install_signal_handlers;