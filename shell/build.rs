fn main() {
    // Pin the exported names; see exports.def.
    let def = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("exports.def");
    println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def.display());
    println!("cargo:rerun-if-changed=exports.def");
}
