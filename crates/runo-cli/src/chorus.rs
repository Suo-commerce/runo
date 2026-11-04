//! Tier 3 setup (technical spec §10, §11): `runo enroll` on each host, then `runo init` once by
//! the owner. The chorus directory holds only public files and can be copied to every host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use runo_core::hash::{digest, display};
use runo_node::identity::{ChorusDir, KeyHome};
use runo_profile::manifest::{Enrolment, check_chorus, load_manifest, roster};
use runo_profile::{Profile, load_profile};

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// `runo enroll`: make this voice's key in the key home and write its enrolment.
pub fn enroll(singer: &str, chorus: &Path, credential: Option<&str>) -> Result<(), String> {
    let home = KeyHome::from_env();
    let dir = ChorusDir::at(chorus);
    let salt = dir.salt().map_err(|e| e.to_string())?;
    let key = home.generate(singer).map_err(|e| e.to_string())?;
    let reference = credential.map_or_else(|| format!("file://keys/{singer}.key"), str::to_owned);
    let public = key.verifying_key().to_bytes();
    let path = dir
        .write_enrolment(&Enrolment::new(singer, public, &salt, &reference))
        .map_err(|e| e.to_string())?;
    println!("enrolled {singer}");
    println!("  public key   {}", hex(&public));
    println!("  enrolment    {}", path.display());
    println!("  the secret key stays in this host's key home and is never printed");
    Ok(())
}

/// `chorus.toml` and each voice's profile, keyed by the manifest's `profile` path.
pub fn load(
    manifest: &Path,
) -> Result<(runo_profile::manifest::Manifest, BTreeMap<String, Profile>), String> {
    let text =
        std::fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let m = load_manifest(&text).map_err(|e| e.to_string())?;
    let base = manifest.parent().unwrap_or(Path::new("."));
    let mut profiles = BTreeMap::new();
    for v in &m.voices {
        if profiles.contains_key(&v.profile) {
            continue;
        }
        let path: PathBuf = base.join(&v.profile);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let p = load_profile(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        profiles.insert(v.profile.clone(), p);
    }
    Ok((m, profiles))
}

/// `runo init`: the load-time checks, then roster version 1 from the enrolments, signed by the
/// owner when `sign` is set (the owner's key must be in this host's key home).
pub fn init(manifest: &Path, chorus: &Path, sign: bool) -> Result<(), String> {
    let (m, profiles) = load(manifest)?;
    check_chorus(&m, &profiles).map_err(|e| e.to_string())?;
    println!("chorus {}: the load-time checks pass", m.name);
    let dir = ChorusDir::at(chorus);
    let enrolled = dir.enrolments().map_err(|e| e.to_string())?;
    let doc = roster(&m, &profiles, &enrolled).map_err(|e| e.to_string())?;
    for v in &doc.voices {
        println!(
            "  {:<10} {:<9} host {:<16} key {}…",
            v.singer.0,
            v.role,
            v.domains.host,
            &hex(&v.key)[..16]
        );
    }
    let bytes = runo_core::codec::canonical(&doc).map_err(|e| e.to_string())?;
    println!("roster v1 {}", display(&digest(&bytes)));
    if !sign {
        println!("not signed: run again with --sign on the host that holds the owner's key");
        return Ok(());
    }
    let owner = KeyHome::from_env()
        .load(&doc.owner.0)
        .map_err(|e| e.to_string())?;
    let path = dir.write_roster(&doc, &owner).map_err(|e| e.to_string())?;
    println!(
        "signed by {} and written to {}",
        doc.owner.0,
        path.display()
    );
    println!(
        "pin this owner key on every host: {}",
        hex(&owner.verifying_key().to_bytes())
    );
    Ok(())
}
