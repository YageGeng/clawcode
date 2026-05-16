//! Local terminal UI for clawcode.
//!
//! The TUI starts the local ACP agent in-process and renders ACP session
//! notifications with ratatui.

pub mod acp_client;
pub mod acp_server;
pub mod app;
pub mod event;
pub mod terminal;
pub mod ui;
