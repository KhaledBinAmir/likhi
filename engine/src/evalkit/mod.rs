//! The evaluation harness: what measures whether a ranking change helped.
//!
//! A port of the word-level half of `src/likhi/eval/`. It exists in Rust because it is the loop
//! every ranking change goes through, and in Python it drives the engine one word at a time -- a
//! run over dakshina-dev takes about twelve minutes, which is long enough to discourage measuring.
//!
//! Behind the `tools` feature, so none of it is compiled into the engine that ships.

pub mod datasets;
pub mod metrics;
pub mod pyrandom;
pub mod sentences;
pub mod styles;
