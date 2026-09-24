//! The Likhi suggestion engine.
//!
//! Originally a port of a Python engine, module for module and name for name. That Python is gone:
//! everything it did -- the engine, the lexicon builder, the evaluation harness, the tuner, the
//! stress tool, the telemetry operator tool -- now lives here, and this crate is the only
//! implementation.
//!
//! The port existed to remove an embedded CPython and NumPy, about 65 MB and a cold start, from
//! every installation, and not to change any behaviour. Correctness was therefore defined as exact
//! agreement with the Python, and that agreement is what `tests/goldens.rs` still asserts: 96,537
//! recorded cases covering every module, where string and integer results must match to the
//! character. Only the transformer's float32 outputs were ever allowed to differ, and only in the
//! last bits, because a different summation order is unavoidable; those are checked on ranking,
//! which is what a typist actually experiences.
//!
//! The goldens can no longer be regenerated, which is the price of the deletion and is deliberate:
//! they are now a fixed record of behaviour that was verified against a second implementation, and
//! a change that moves them has to be justified on its own terms rather than by re-recording.

pub mod avro;
pub mod core;
/// The evaluation harness. Behind `tools`, so it is absent from the shipped engine.
#[cfg(feature = "tools")]
pub mod evalkit;
pub mod http;
pub mod lexicon;
pub mod personal;
#[cfg(windows)]
pub mod pipe;
pub mod service;
pub mod telemetry;
/// The candidate window drawn on behalf of a sandboxed application, which cannot show one itself.
#[cfg(windows)]
pub mod uihost;
/// Signed self-update: manifest verification, version comparison, and the daily check.
pub mod update;
/// Offering a verified update to the person: a notification they click to install.
#[cfg(windows)]
pub mod notify;
pub mod romankey;
pub mod tensor;
pub mod textnorm;
pub mod weights;
pub mod xlit;
