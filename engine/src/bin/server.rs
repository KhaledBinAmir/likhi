//! The Likhi engine server: JSON lines over a named pipe and a local socket.
//!
//! Not yet implemented. The port is being built bottom-up -- normalization, lexicon, phonetic keys,
//! the transliteration model, ranking -- and this becomes the front door once there is an engine
//! behind it. Until then `src/likhi/server.py` is what runs, and the protocol it speaks is the
//! contract this must meet: see `docs/port/server.md`.

fn main() {
    eprintln!(
        "likhi-server (Rust) is not finished yet; the Python engine in src/likhi/server.py is \
         still the one that runs. See docs/port/ for the porting specifications."
    );
    std::process::exit(1);
}
