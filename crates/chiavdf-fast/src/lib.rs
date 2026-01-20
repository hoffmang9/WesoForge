#![deny(missing_docs)]
#![deny(unreachable_pub)]

//! Minimal Rust wrapper around a fast chiavdf C API.

/// Public API for this crate.
pub mod api;

mod ffi;

pub use api::{prove_one_weso_fast, prove_one_weso_fast_with_progress, ChiavdfFastError};
