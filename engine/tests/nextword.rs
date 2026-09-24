//! Next-word suggestions against the real tables: what the strip offers, and what it must not.
//!
//! These pin behaviour a person sees after pressing Space, so they use the shipped data rather than
//! a fixture -- a fixture would test the threshold arithmetic and miss the table saying something
//! embarrassing, which is how the one exclusion so far was found.

use std::path::{Path, PathBuf};

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("models")
        .join("rust")
}

fn open_engine() -> Option<likhi_engine::core::Engine> {
    let dir = data_dir();
    if !dir.join("indicxlit").join("model.lkw").exists() {
        eprintln!("SKIPPING next-word tests: run scripts/build_rust_data.py first");
        return None;
    }
    let opts = likhi_engine::core::EngineOptions { personal: None, ..Default::default() };
    Some(likhi_engine::core::Engine::open(&dir, opts).expect("engine opens"))
}

#[test]
fn a_confident_collocation_is_offered() {
    let Some(e) = open_engine() else { return };
    // দেখা যায় -- "can be seen" -- is over half of everything after দেখা in the table. Compared by
    // match key: য় has two encodings, precomposed and য plus nukta, and which one a literal in this
    // file uses is up to the editor that saved it.
    let offered = e.next_words("দেখা", 1, 0.2);
    let keys: Vec<String> = offered.iter().map(|w| likhi_engine::textnorm::match_key(w)).collect();
    assert_eq!(keys, vec![likhi_engine::textnorm::match_key("যায়")], "offered {offered:?}");
}

#[test]
fn a_common_word_with_no_clear_follower_gets_nothing() {
    let Some(e) = open_engine() else { return };
    // Many different words follow আমি and none holds a fifth of them, so nothing is shown: a guess
    // that is usually wrong is what the threshold exists to keep off the screen.
    assert!(e.next_words("আমি", 1, 0.2).is_empty());
    // Below the threshold there are plenty of candidates, so the empty answer is the threshold's
    // doing and not a lookup failure.
    assert!(!e.next_words("আমি", 3, 0.0).is_empty());
}

#[test]
fn the_username_placeholder_is_never_offered() {
    let Some(e) = open_engine() else { return };
    // The table's top continuation after this greeting is ইউজার, which stood in for a username in
    // the forum text it was built from. See NEVER_SUGGESTED_NEXT.
    for greeting in ["আসসালামুআলাইকুম", "হেলো", "তুমার"] {
        let offered = e.next_words(greeting, 5, 0.0);
        assert!(
            !offered.iter().any(|w| w == "ইউজার"),
            "offered the placeholder after {greeting}: {offered:?}"
        );
    }
}

#[test]
fn an_unknown_word_gets_nothing() {
    let Some(e) = open_engine() else { return };
    assert!(e.next_words("ঝ্ক্ষ্ট", 3, 0.0).is_empty());
    assert!(e.next_words("", 3, 0.0).is_empty());
}
