//! Embedded PTY terminal subsystem.
//!
//! Wraps `portable-pty` so the frontend can spawn a real shell (PowerShell or
//! cmd.exe on Windows, `$SHELL`/bash on POSIX — see `pty::default_shell_program`)
//! and round-trip raw bytes over a Tauri event channel. The Rust side owns the PTY master + child process; the
//! React side (xterm.js) is just a renderer.
//!
//! See `pty.rs` for the open/write/resize/close primitives and
//! `commands/terminal.rs` for the Tauri command wrappers that talk to JS.

pub mod pty;
