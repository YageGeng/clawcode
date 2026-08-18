//! Provider-boundary hook contracts shared across runtime layers.
//!
//! Each submodule defines the hook contract consumed at one runtime boundary.
//! `model` holds the provider request/response hooks accepted by the kernel's
//! `Model` trait and implemented by the provider adapters; future boundaries
//! (tools, sessions, skills) would add their own submodules here.

pub mod model;
