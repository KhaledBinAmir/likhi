//! `likhi-sign`: the only program that touches the update signing key.
//!
//!     likhi-sign keygen   [--key <file>]
//!     likhi-sign manifest --installer <exe> --version <x.y.z> [--key <file>] [--out <dir>]
//!     likhi-sign verify   --manifest <latest.json> [--installer <exe>]
//!
//! The private key is a 32-byte seed in hex, kept outside the repository at
//! `%USERPROFILE%\.likhi\update-signing.key` by default. It is never printed. Losing it does not
//! break anything already installed, but it does mean no further update can reach those machines
//! until someone installs a build that trusts a new key by hand -- so it is worth backing up.
//!
//! `manifest` writes `latest.json` and `latest.json.sig` beside each other; both are published as
//! release assets next to the installer. `verify` checks a manifest against the keys compiled into
//! this build, which are the same keys the engine trusts, so a release can be checked end to end
//! before anyone downloads it.

use std::path::{Path, PathBuf};

use ed25519_compact::{KeyPair, Seed};
use likhi_engine::update::{
    hex_decode, hex_encode, safe_file_name, sha512_hex, verify_installer, verify_manifest,
    Manifest,
};

fn default_key() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".likhi").join("update-signing.key")
}

fn load_keypair(path: &Path) -> Result<KeyPair, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read the signing key at {}: {e}", path.display()))?;
    let seed = hex_decode(text.trim())?;
    let seed = Seed::from_slice(&seed).map_err(|e| format!("the key file is not a seed: {e}"))?;
    Ok(KeyPair::from_seed(seed))
}

fn cmd_keygen(key: &Path) -> Result<(), String> {
    // Never overwrite. A second keygen over an existing key would silently orphan every install
    // that trusts the first one, and there is no undo.
    if key.exists() {
        return Err(format!(
            "a signing key already exists at {}; refusing to replace it. Move it aside first if \
             you really mean to start over -- installs that trust it will stop receiving updates.",
            key.display()
        ));
    }
    if let Some(dir) = key.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let seed = Seed::generate();
    let kp = KeyPair::from_seed(seed);
    // Written with create_new so a race with another keygen cannot clobber either one.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(key)
        .map_err(|e| format!("cannot create {}: {e}", key.display()))?;
    writeln!(f, "{}", hex_encode(seed.as_ref())).map_err(|e| e.to_string())?;
    drop(f);
    restrict_to_owner(key);

    println!("signing key written to {}", key.display());
    println!("back it up: without it no future update can reach machines that trust it");
    println!();
    println!("public key (put this in TRUSTED_KEYS in engine/src/update.rs):");
    println!("{}", hex_encode(kp.pk.as_ref()));
    Ok(())
}

/// Remove inherited permissions so only the current user can read the key.
///
/// The account is named as DOMAIN\user, never as a bare user name. On the development machine the
/// computer is called KHALED and the user khaled, and a bare "khaled" resolved to the computer's
/// account: the first key this wrote was readable by nobody at all, and had to be recovered by
/// resetting the permissions as its owner. With inheritance removed, a wrong name here is not a
/// weaker lock, it is a locked-out key.
///
/// If the qualified name is unavailable nothing is changed. The file is already under the user's
/// profile, which other ordinary users cannot read, so leaving it alone is safe; guessing is not.
fn restrict_to_owner(path: &Path) {
    let (Ok(domain), Ok(user)) = (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) else {
        return;
    };
    if domain.is_empty() || user.is_empty() {
        return;
    }
    let account = format!("{domain}\\{user}");
    let ok = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{account}:F"))
        .status()
        .is_ok_and(|s| s.success());
    // Prove it rather than trust it. If the key cannot be read back, restore inheritance so it is at
    // least readable again, and say so.
    if !ok || std::fs::read(path).is_err() {
        let _ = std::process::Command::new("icacls").arg(path).arg("/reset").status();
        eprintln!(
            "warning: could not restrict {} to {account}; left with the folder's permissions",
            path.display()
        );
    }
}

fn cmd_manifest(installer: &Path, version: &str, key: &Path, out: &Path) -> Result<(), String> {
    let kp = load_keypair(key)?;
    let bytes = std::fs::read(installer)
        .map_err(|e| format!("cannot read {}: {e}", installer.display()))?;
    let name = installer
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("the installer path has no file name")?
        .to_string();
    if !safe_file_name(&name) {
        return Err(format!("{name:?} is not a name the engine will accept"));
    }
    let manifest = Manifest {
        version: version.trim().trim_start_matches('v').to_string(),
        installer: name,
        size: bytes.len() as u64,
        sha512: sha512_hex(&bytes),
    };
    // These exact bytes are what gets signed and what the engine verifies. Nothing re-serialises
    // them later, so pretty-printing is safe and makes the file readable on the release page.
    let body = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let sig = kp.sk.sign(&body, None);

    std::fs::create_dir_all(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let mpath = out.join("latest.json");
    let spath = out.join("latest.json.sig");
    std::fs::write(&mpath, &body).map_err(|e| e.to_string())?;
    std::fs::write(&spath, format!("{}\n", hex_encode(sig.as_ref()))).map_err(|e| e.to_string())?;

    // Check what was just written with the keys this build trusts, exactly as the engine will. A
    // manifest signed with a key the engine does not trust would be published and then silently
    // ignored by every install; better to find out here.
    let written = std::fs::read(&mpath).map_err(|e| e.to_string())?;
    let sig_text = std::fs::read_to_string(&spath).map_err(|e| e.to_string())?;
    let checked = verify_manifest(&written, &sig_text)
        .map_err(|e| format!("the manifest was written but does not verify: {e}"))?;
    verify_installer(&bytes, &checked)?;

    println!("{}", mpath.display());
    println!("{}", spath.display());
    println!("version {}  size {}  sha512 {}...", manifest.version, manifest.size, &manifest.sha512[..16]);
    println!("verified against the keys compiled into this build");
    Ok(())
}

fn cmd_verify(manifest: &Path, installer: Option<&Path>) -> Result<(), String> {
    let body = std::fs::read(manifest).map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    let sig_path = PathBuf::from(format!("{}.sig", manifest.display()));
    let sig = std::fs::read_to_string(&sig_path)
        .map_err(|e| format!("cannot read {}: {e}", sig_path.display()))?;
    let m = verify_manifest(&body, &sig)?;
    println!("signature valid: version {} ({})", m.version, m.installer);
    if let Some(path) = installer {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        verify_installer(&bytes, &m)?;
        println!("installer matches the signed size and hash");
    }
    Ok(())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let key = flag(&args, "--key").map(PathBuf::from).unwrap_or_else(default_key);
    let result = match args.get(1).map(String::as_str) {
        Some("keygen") => cmd_keygen(&key),
        Some("manifest") => match (flag(&args, "--installer"), flag(&args, "--version")) {
            (Some(inst), Some(ver)) => {
                let out = flag(&args, "--out").map(PathBuf::from).unwrap_or_else(|| {
                    Path::new(&inst).parent().map(Path::to_path_buf).unwrap_or_default()
                });
                cmd_manifest(Path::new(&inst), &ver, &key, &out)
            }
            _ => Err("manifest needs --installer and --version".into()),
        },
        Some("verify") => match flag(&args, "--manifest") {
            Some(m) => cmd_verify(Path::new(&m), flag(&args, "--installer").as_deref().map(Path::new)),
            None => Err("verify needs --manifest".into()),
        },
        _ => Err("usage:\n  likhi-sign keygen [--key <file>]\n  likhi-sign manifest --installer <exe> --version <x.y.z> [--key <file>] [--out <dir>]\n  likhi-sign verify --manifest <latest.json> [--installer <exe>]".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
