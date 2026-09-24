//! Signed self-update.
//!
//! An updater is a channel through which code arrives on every machine that runs Likhi and is then
//! run with administrator rights. If the release channel is ever compromised -- the GitHub account,
//! a token, a release edited after the fact -- whatever arrives through it runs everywhere. So
//! nothing is trusted because of where it came from. It is trusted because it is signed by a key
//! that has never left the publisher's machine, and the public half of that key is compiled in here.
//!
//! What is signed is a small manifest, not the installer:
//!
//! ```text
//! {"version": "0.5.1", "installer": "LikhiSetup-0.5.1.exe", "size": 40586793, "sha512": "..."}
//! ```
//!
//! The engine checks the manifest's signature, then checks the downloaded installer against the
//! size and hash the manifest names. Signing the manifest rather than the file keeps verification
//! cheap, and -- more importantly -- binds the version into the signed statement: an old installer
//! that was validly signed in its day cannot be replayed as an upgrade, because the engine only acts
//! on a signed version newer than its own.
//!
//! Everything here fails closed. A version that does not parse, a signature from an unknown key, a
//! file whose hash differs by a bit: each is a reason to do nothing, never a reason to guess.

use ed25519_compact::{PublicKey, Signature};

/// This build's version, set by `scripts/build_engine.py` from the installer's version.
///
/// `None` for a development build, and the updater stays off for those. Otherwise a build made with
/// plain `cargo build` would report some placeholder version, find every published release newer,
/// and offer to replace itself with an older engine.
pub const PRODUCT_VERSION: Option<&str> = option_env!("LIKHI_VERSION");

/// Public keys whose signatures are accepted, as hex.
///
/// A list rather than one key so the signing key can be replaced without stranding anyone: ship a
/// build that trusts both the old key and the new one, wait until it has reached everybody, then
/// start signing with the new one. The private half of each lives only on the publisher's machine,
/// outside the repository.
pub const TRUSTED_KEYS: &[&str] = &[
    // Generated 2026-09-24 by `likhi-sign keygen`. Private half: %USERPROFILE%\.likhi\update-signing.key
    "bbc6e080f86eb0f65ede93d202d273c5b30dea35c27a102882e3d9665a44b2d9",
];

/// The signed statement about one release.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Manifest {
    pub version: String,
    /// The release asset to download. Used only to pick the asset; acceptance is by hash.
    pub installer: String,
    pub size: u64,
    /// SHA-512 of the installer, lowercase hex.
    pub sha512: String,
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

pub fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return Err("hex has an odd number of digits".into());
    }
    let nibble = |c: u8| -> Result<u8, String> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(format!("not a hex digit: {:?}", c as char)),
        }
    };
    text.as_bytes()
        .chunks(2)
        .map(|pair| Ok(nibble(pair[0])? << 4 | nibble(pair[1])?))
        .collect()
}

/// SHA-512 of `bytes`, lowercase hex.
pub fn sha512_hex(bytes: &[u8]) -> String {
    hex_encode(&ed25519_compact::sha512::Hash::hash(bytes))
}

/// Check the manifest's signature against every trusted key and parse it only if one matches.
///
/// `manifest` must be the exact bytes that were signed, as downloaded. Nothing is re-serialised
/// before checking: two JSON encoders that disagree about whitespace would otherwise produce a
/// signature that fails for no reason, or one that passes over bytes nobody signed.
pub fn verify_manifest(manifest: &[u8], signature_hex: &str) -> Result<Manifest, String> {
    verify_manifest_with(manifest, signature_hex, TRUSTED_KEYS)
}

/// `verify_manifest` against an explicit key list, so the tests can use a key of their own.
pub fn verify_manifest_with(
    manifest: &[u8],
    signature_hex: &str,
    keys: &[&str],
) -> Result<Manifest, String> {
    let sig_bytes = hex_decode(signature_hex)?;
    let signature =
        Signature::from_slice(&sig_bytes).map_err(|e| format!("malformed signature: {e}"))?;
    let signed_by_trusted = keys.iter().any(|k| {
        hex_decode(k)
            .ok()
            .and_then(|pk| PublicKey::from_slice(&pk).ok())
            .is_some_and(|pk| pk.verify(manifest, &signature).is_ok())
    });
    if !signed_by_trusted {
        return Err("the manifest is not signed by any trusted key".into());
    }
    let m: Manifest =
        serde_json::from_slice(manifest).map_err(|e| format!("signed but unreadable: {e}"))?;
    if !safe_file_name(&m.installer) {
        return Err(format!("installer name {:?} is not a plain file name", m.installer));
    }
    if hex_decode(&m.sha512).map(|h| h.len()).unwrap_or(0) != 64 {
        return Err("the manifest's sha512 is not 64 bytes of hex".into());
    }
    Ok(m)
}

/// The downloaded installer is exactly the one the signed manifest describes.
pub fn verify_installer(bytes: &[u8], m: &Manifest) -> Result<(), String> {
    if bytes.len() as u64 != m.size {
        return Err(format!("size {} does not match the signed {}", bytes.len(), m.size));
    }
    let got = sha512_hex(bytes);
    if !got.eq_ignore_ascii_case(m.sha512.trim()) {
        return Err("the installer's hash does not match the signed manifest".into());
    }
    Ok(())
}

/// A name that is safe to use as the last component of a path we write to.
///
/// The name comes from the network. It is signed, so this is defence in depth rather than the only
/// line, but a path component is the wrong place to discover that a key was stolen.
pub fn safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(|c| c.is_control())
        && name.to_ascii_lowercase().ends_with(".exe")
}

/// `a.b.c` as numbers. Anything else -- a pre-release suffix, a fourth part, a blank -- does not
/// parse, and something that does not parse is never treated as newer.
fn parse_version(v: &str) -> Option<[u64; 3]> {
    let mut parts = v.trim().trim_start_matches('v').split('.');
    let mut out = [0u64; 3];
    for slot in out.iter_mut() {
        *slot = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

/// True only when `candidate` is strictly newer than `current` and both parse.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

// ----------------------------------------------------------------------------------- the check

/// A verified installer on disk, newer than this build.
#[derive(Debug, Clone)]
pub struct Ready {
    pub manifest: Manifest,
    pub path: std::path::PathBuf,
}

/// Where the releases are listed. Overridable so the whole path can be exercised against a local
/// stand-in; that does not weaken anything, because what is accepted is decided by the signature
/// and not by where it came from.
fn releases_url() -> String {
    std::env::var("LIKHI_UPDATE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            "https://api.github.com/repos/KhaledBinAmir/likhi/releases?per_page=10".to_string()
        })
}

/// A release asset: (file name, download url).
type Asset = (String, String);

/// The release chosen for an update: where its manifest and signature are, and all its assets.
struct Picked {
    manifest_url: String,
    sig_url: String,
    assets: Vec<Asset>,
}

/// The newest release that carries a signed manifest.
///
/// Pre-releases count -- every Likhi release so far has been one -- and drafts do not. GitHub lists
/// releases newest first. A release without a manifest is one published before self-update
/// existed, or by hand, and is skipped rather than guessed at.
fn pick_release(listing: &serde_json::Value) -> Option<Picked> {
    for rel in listing.as_array()? {
        if rel.get("draft").and_then(|d| d.as_bool()).unwrap_or(false) {
            continue;
        }
        let assets: Vec<Asset> = rel
            .get("assets")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        Some((
                            x.get("name")?.as_str()?.to_string(),
                            x.get("browser_download_url")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let url_of = |name: &str| assets.iter().find(|(n, _)| n == name).map(|(_, u)| u.clone());
        if let (Some(manifest_url), Some(sig_url)) = (url_of("latest.json"), url_of("latest.json.sig")) {
            return Some(Picked { manifest_url, sig_url, assets });
        }
    }
    None
}

#[cfg(windows)]
fn fetch(url: &str, accept: &str) -> Result<Vec<u8>, String> {
    crate::http::get(
        url,
        &[
            ("Accept", accept.to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ],
        crate::http::EXPORT_TIMEOUT_MS,
    )
}

#[cfg(not(windows))]
fn fetch(_url: &str, _accept: &str) -> Result<Vec<u8>, String> {
    Err("updates are implemented only on Windows".into())
}

/// One check: find the newest signed release, and if it is newer than `current`, make sure a
/// verified copy of its installer is in `dir`.
///
/// Returns `Ok(None)` when this build is up to date. Everything that could be wrong -- the listing,
/// the signature, the hash -- is an error, and an error means nothing is offered.
pub fn check_once(current: &str, dir: &std::path::Path) -> Result<Option<Ready>, String> {
    let listing = fetch(&releases_url(), "application/vnd.github+json")?;
    let listing: serde_json::Value =
        serde_json::from_slice(&listing).map_err(|e| format!("release listing: {e}"))?;
    let Some(Picked { manifest_url, sig_url, assets }) = pick_release(&listing) else {
        return Ok(None);
    };
    let manifest = fetch(&manifest_url, "application/octet-stream")?;
    let sig = fetch(&sig_url, "application/octet-stream")?;
    let sig = String::from_utf8_lossy(&sig);
    let m = verify_manifest(&manifest, &sig)?;
    if !is_newer(&m.version, current) {
        return Ok(None);
    }

    let path = dir.join(&m.installer);
    // Already downloaded on an earlier check. Verified again rather than trusted: the file sits in
    // a folder the user can write to, and yesterday's copy is only as good as today's hash says.
    if let Ok(bytes) = std::fs::read(&path) {
        if verify_installer(&bytes, &m).is_ok() {
            return Ok(Some(Ready { manifest: m, path }));
        }
    }
    let url = assets
        .iter()
        .find(|(n, _)| *n == m.installer)
        .map(|(_, u)| u.clone())
        .ok_or_else(|| format!("the signed manifest names {} but the release has no such asset", m.installer))?;
    let bytes = fetch(&url, "application/octet-stream")?;
    verify_installer(&bytes, &m)?;

    // Written under a temporary name and renamed into place only once complete and verified, so a
    // download interrupted halfway never leaves something that looks like a finished installer.
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let partial = dir.join(format!("{}.partial", m.installer));
    std::fs::write(&partial, &bytes).map_err(|e| format!("cannot write the download: {e}"))?;
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&partial, &path).map_err(|e| format!("cannot finish the download: {e}"))?;
    Ok(Some(Ready { manifest: m, path }))
}

/// Remove downloaded installers that are no longer needed: after an update has been installed, or
/// when the newest release turns out not to be newer after all.
pub fn clear_downloads(dir: &std::path::Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The folder downloads go to, beside the rest of the engine's per-user state.
pub fn download_dir() -> std::path::PathBuf {
    crate::telemetry::local_app_data().join("Likhi").join("updates")
}

/// Whether the person wants updates checked for. On unless they said otherwise.
///
/// Separate from usage reporting on purpose. A daily check sends nothing about what anyone types,
/// but it does tell GitHub that a machine at this address runs Likhi, so turning reporting off must
/// not silently leave this on, and turning updates off must not need reporting off too.
pub fn enabled() -> bool {
    crate::telemetry::shell_config()
        .and_then(|c| c.get("check_updates").and_then(|v| v.as_bool()))
        .unwrap_or(true)
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct State {
    /// Unix seconds of the last completed check.
    last_check: u64,
    /// The version last offered, and when, so a declined update is offered again tomorrow rather
    /// than on every hourly pass.
    notified_version: String,
    notified_at: u64,
}

fn state_path() -> std::path::PathBuf {
    crate::telemetry::local_app_data().join("Likhi").join("update.json")
}

fn load_state() -> State {
    std::fs::read(state_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_state(s: &State) {
    if let Ok(text) = serde_json::to_vec_pretty(s) {
        let _ = std::fs::write(state_path(), text);
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

const DAY: u64 = 24 * 60 * 60;

/// Held for the whole of a check, by the daily loop and by "check now" alike. Two checks at once
/// would download the same installer into the same temporary file.
static CHECKING: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Set by `spawn`, so a manual check can offer what it finds the same way the daily one does.
static OFFER: std::sync::OnceLock<fn(&Ready) -> bool> = std::sync::OnceLock::new();
static LOG: std::sync::OnceLock<fn(&str)> = std::sync::OnceLock::new();

/// What a manual check found, in a form the Likhi window can show.
#[derive(Debug, serde::Serialize)]
pub struct CheckResult {
    pub ok: bool,
    /// "up_to_date", "available", "failed" or "dev".
    pub status: &'static str,
    pub current: String,
    pub version: Option<String>,
    pub error: Option<String>,
}

/// Check right now, because someone asked.
///
/// Ignores the once-a-day schedule and the "check for updates" setting: clicking "check now" is an
/// explicit request, and a setting meant to stop the engine asking GitHub by itself should not stop
/// a person from asking. Everything else is the daily check exactly: the same signature, the same
/// hash, the same offer.
pub fn check_now() -> CheckResult {
    let Some(current) = PRODUCT_VERSION else {
        return CheckResult {
            ok: true,
            status: "dev",
            current: "dev".into(),
            version: None,
            error: Some("a development build does not update".into()),
        };
    };
    let _held = CHECKING.lock().unwrap_or_else(|e| e.into_inner());
    let result = check_once(current, &download_dir());
    let mut state = load_state();
    state.last_check = now();
    let out = match result {
        Ok(Some(ready)) => {
            let shown = OFFER.get().is_some_and(|offer| offer(&ready));
            if shown {
                state.notified_version = ready.manifest.version.clone();
                state.notified_at = now();
            }
            CheckResult {
                ok: true,
                status: "available",
                current: current.into(),
                version: Some(ready.manifest.version),
                error: None,
            }
        }
        Ok(None) => CheckResult {
            ok: true,
            status: "up_to_date",
            current: current.into(),
            version: None,
            error: None,
        },
        Err(e) => {
            if let Some(log) = LOG.get() {
                log(&format!("updates: manual check failed ({e})"));
            }
            CheckResult { ok: false, status: "failed", current: current.into(), version: None, error: Some(e) }
        }
    };
    save_state(&state);
    out
}

/// Start the daily check on a thread of its own. Does nothing in a development build.
///
/// `offer` puts a verified update in front of the person and returns whether it was shown. It is a
/// parameter rather than a direct call so this module has no opinion about how that looks.
pub fn spawn(log: fn(&str), offer: fn(&Ready) -> bool) {
    let _ = OFFER.set(offer);
    let _ = LOG.set(log);
    let Some(current) = PRODUCT_VERSION else {
        log("updates: development build, not checking");
        return;
    };
    let _ = std::thread::Builder::new()
        .name("likhi-update".into())
        .spawn(move || run(current, log, offer));
}

fn run(current: &'static str, log: fn(&str), offer: fn(&Ready) -> bool) {
    // Not at startup. Sign-in is when every machine in an office starts at once, and when a
    // computer is busiest; a check has no reason to join that queue. Staggered by process id and
    // clock so a room full of machines does not ask in the same second.
    let jitter = (std::process::id() as u64 ^ now()) % 600;
    let first = if std::env::var_os("LIKHI_UPDATE_NOW").is_some() { 3 } else { 600 + jitter };
    std::thread::sleep(std::time::Duration::from_secs(first));

    let dir = download_dir();
    let mut ready: Option<Ready> = None;
    loop {
        if enabled() {
            let mut state = load_state();
            if now().saturating_sub(state.last_check) >= DAY || ready.is_none() && state.last_check == 0 {
                let held = CHECKING.lock().unwrap_or_else(|e| e.into_inner());
                let checked = check_once(current, &dir);
                drop(held);
                match checked {
                    Ok(Some(r)) => {
                        log(&format!("updates: {} is available and verified ({})", r.manifest.version, r.path.display()));
                        ready = Some(r);
                    }
                    Ok(None) => {
                        log(&format!("updates: {current} is the newest signed release"));
                        ready = None;
                        clear_downloads(&dir);
                    }
                    Err(e) => log(&format!("updates: check failed, nothing offered ({e})")),
                }
                state.last_check = now();
                save_state(&state);
            }
            if let Some(r) = &ready {
                let offered_recently = state.notified_version == r.manifest.version
                    && now().saturating_sub(state.notified_at) < DAY;
                if !offered_recently && offer(r) {
                    state.notified_version = r.manifest.version.clone();
                    state.notified_at = now();
                    save_state(&state);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(60 * 60));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_compact::{KeyPair, Seed};

    fn keypair(seed_byte: u8) -> KeyPair {
        KeyPair::from_seed(Seed::new([seed_byte; 32]))
    }

    fn signed(kp: &KeyPair, manifest: &[u8]) -> String {
        hex_encode(kp.sk.sign(manifest, None).as_ref())
    }

    fn manifest_for(bytes: &[u8], version: &str) -> Vec<u8> {
        serde_json::to_vec(&Manifest {
            version: version.into(),
            installer: format!("LikhiSetup-{version}.exe"),
            size: bytes.len() as u64,
            sha512: sha512_hex(bytes),
        })
        .unwrap()
    }

    #[test]
    fn a_manifest_signed_by_a_trusted_key_is_accepted() {
        let kp = keypair(7);
        let pk = hex_encode(kp.pk.as_ref());
        let installer = b"pretend this is an installer";
        let m = manifest_for(installer, "0.5.1");
        let got = verify_manifest_with(&m, &signed(&kp, &m), &[&pk]).expect("valid");
        assert_eq!(got.version, "0.5.1");
        verify_installer(installer, &got).expect("the file matches");
    }

    /// The case this module exists for.
    #[test]
    fn a_manifest_signed_by_anyone_else_is_refused() {
        let ours = keypair(7);
        let theirs = keypair(9);
        let m = manifest_for(b"malware", "9.9.9");
        let err = verify_manifest_with(&m, &signed(&theirs, &m), &[&hex_encode(ours.pk.as_ref())])
            .unwrap_err();
        assert!(err.contains("not signed by any trusted key"), "{err}");
    }

    #[test]
    fn one_changed_byte_in_the_manifest_is_refused() {
        let kp = keypair(7);
        let pk = hex_encode(kp.pk.as_ref());
        let m = manifest_for(b"installer", "0.5.1");
        let sig = signed(&kp, &m);
        let mut tampered = m.clone();
        // Bump the version in place: same length, still valid JSON, no longer what was signed.
        let pos = tampered.windows(5).position(|w| w == b"0.5.1").unwrap();
        tampered[pos + 4] = b'2';
        assert!(verify_manifest_with(&tampered, &sig, &[&pk]).is_err());
    }

    #[test]
    fn an_installer_that_differs_from_the_signed_hash_is_refused() {
        let kp = keypair(7);
        let pk = hex_encode(kp.pk.as_ref());
        let genuine = b"the real installer";
        let m = verify_manifest_with(&manifest_for(genuine, "0.5.1"), &signed(&kp, &manifest_for(genuine, "0.5.1")), &[&pk]).unwrap();
        let swapped = b"the real installeR"; // same length, one bit different
        assert!(verify_installer(swapped, &m).unwrap_err().contains("hash"));
        assert!(verify_installer(b"short", &m).unwrap_err().contains("size"));
    }

    #[test]
    fn a_second_trusted_key_lets_the_signing_key_be_rotated() {
        let old = keypair(1);
        let new = keypair(2);
        let keys = [hex_encode(old.pk.as_ref()), hex_encode(new.pk.as_ref())];
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let m = manifest_for(b"x", "0.6.0");
        assert!(verify_manifest_with(&m, &signed(&old, &m), &keys).is_ok());
        assert!(verify_manifest_with(&m, &signed(&new, &m), &keys).is_ok());
    }

    #[test]
    fn a_signed_path_is_still_not_allowed_to_escape() {
        let kp = keypair(7);
        let pk = hex_encode(kp.pk.as_ref());
        let evil = serde_json::to_vec(&serde_json::json!({
            "version": "0.5.1", "installer": "..\\..\\evil.exe", "size": 1,
            "sha512": sha512_hex(b"x"),
        }))
        .unwrap();
        let err = verify_manifest_with(&evil, &signed(&kp, &evil), &[&pk]).unwrap_err();
        assert!(err.contains("plain file name"), "{err}");
    }

    #[test]
    fn versions_compare_as_numbers_and_fail_closed() {
        assert!(is_newer("0.5.1", "0.5.0"));
        assert!(is_newer("0.10.0", "0.9.9"), "numeric, not lexical");
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.5.0", "0.5.0"), "equal is not newer");
        assert!(!is_newer("0.4.9", "0.5.0"), "never downgrade");
        assert!(is_newer("v0.5.1", "0.5.0"), "a tag prefix is tolerated");
        for junk in ["0.5", "0.5.1.2", "0.5.1-beta", "", "banana"] {
            assert!(!is_newer(junk, "0.0.1"), "{junk:?} must not count as newer");
        }
    }

    /// The hash every manifest carries has to be the SHA-512 everyone else computes, or a file that
    /// is genuinely correct would be refused. Pinned to the published test vectors from FIPS 180-2
    /// rather than to whatever this implementation happens to produce.
    #[test]
    fn sha512_matches_the_published_test_vectors() {
        assert_eq!(
            sha512_hex(b"abc"),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            sha512_hex(b""),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(hex_decode(&hex_encode(&bytes)).unwrap(), bytes);
        assert!(hex_decode("abc").is_err());
        assert!(hex_decode("zz").is_err());
    }

    #[test]
    fn the_shipped_keys_are_well_formed() {
        // A placeholder or a typo here would mean every update is refused, silently, forever.
        for k in TRUSTED_KEYS {
            let bytes = hex_decode(k).expect("a trusted key is hex");
            assert!(PublicKey::from_slice(&bytes).is_ok(), "{k} is not a valid ed25519 public key");
        }
    }
}
