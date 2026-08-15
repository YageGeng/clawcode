//! Statically compiled built-in Rust extensions.

include!(concat!(env!("OUT_DIR"), "/compiled_extensions.rs"));
