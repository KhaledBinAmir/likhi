//! The Likhi suggestion engine.
//!
//! A port of the Python engine in `src/likhi/`, module for module, with the same names so the two
//! can be read side by side. The Python remains the reference implementation and the research
//! harness (`src/likhi/eval/`) still drives it; this crate is what ships.
//!
//! The port exists to remove an embedded CPython and NumPy -- about 65 MB and a cold start -- from
//! every installation, not to change any behaviour. Correctness is therefore defined as agreement
//! with the Python: `tests/goldens.rs` replays vectors dumped from it by
//! `scripts/dump_goldens.py`, and string and integer results must match exactly. Only the
//! transformer's float32 outputs are allowed to differ, and only in the last bits, because a
//! different summation order is unavoidable; those are checked on ranking, which is what a typist
//! actually experiences.

pub mod avro;
pub mod core;
pub mod lexicon;
pub mod personal;
pub mod romankey;
pub mod tensor;
pub mod textnorm;
pub mod weights;
pub mod xlit;
