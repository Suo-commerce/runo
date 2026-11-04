//! A voice's identity on disk (technical spec §11, key homes; §10, roster generation).
//!
//! - **Key home:** `$RUNO_HOME/keys/<singer>.key`, the 32-byte Ed25519 secret, mode `0600`,
//!   created once and never overwritten, printed nowhere. `$RUNO_HOME` defaults to
//!   `$XDG_DATA_HOME/runo`, else `~/.local/share/runo`.
//! - **Chorus directory:** `salt` (32 random bytes, created by the first enrolment),
//!   `enroll/<singer>.cbor` (an [`Enrolment`]), and `roster.v1.cbor` (the roster and the owner's
//!   signature). Everything in it is public; it can be copied to every host.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use runo_core::RosterDoc;
use runo_core::codec::{canonical, decode};
use runo_core::sign::{generate, sign_roster, verify_roster};
use runo_profile::manifest::Enrolment;
use serde::{Deserialize, Serialize};

/// Identity failure.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// Reading or writing a file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// A key already exists; enrolling twice would orphan the first.
    #[error("{0} already has a key at {1}; remove it yourself if you mean to replace it")]
    KeyExists(String, PathBuf),
    /// A file's contents are not what they should be.
    #[error("{0}: {1}")]
    Bad(PathBuf, String),
    /// The roster's owner signature does not verify, or is not by the pinned owner key.
    #[error("roster: {0}")]
    Untrusted(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> IdentityError + '_ {
    move |source| IdentityError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Where a host keeps its voices' secret keys.
#[derive(Clone, Debug)]
pub struct KeyHome(PathBuf);

impl KeyHome {
    /// `$RUNO_HOME`, else `$XDG_DATA_HOME/runo`, else `~/.local/share/runo`.
    #[must_use]
    pub fn from_env() -> Self {
        let dir = std::env::var_os("RUNO_HOME").map_or_else(
            || {
                std::env::var_os("XDG_DATA_HOME")
                    .map_or_else(
                        || {
                            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                                .join(".local/share")
                        },
                        PathBuf::from,
                    )
                    .join("runo")
            },
            PathBuf::from,
        );
        Self(dir)
    }

    /// A key home at `dir`.
    #[must_use]
    pub fn at(dir: &Path) -> Self {
        Self(dir.to_owned())
    }

    fn path(&self, singer: &str) -> PathBuf {
        self.0.join("keys").join(format!("{singer}.key"))
    }

    /// Generate `singer`'s key, refusing to replace one.
    ///
    /// # Errors
    /// A key exists, or the file cannot be written.
    pub fn generate(&self, singer: &str) -> Result<SigningKey, IdentityError> {
        let path = self.path(singer);
        if path.exists() {
            return Err(IdentityError::KeyExists(singer.to_owned(), path));
        }
        let dir = path.parent().unwrap_or(&self.0).to_owned();
        fs::create_dir_all(&dir).map_err(io(&dir))?;
        let key = generate();
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&path).map_err(io(&path))?;
        f.write_all(&key.to_bytes()).map_err(io(&path))?;
        f.sync_all().map_err(io(&path))?;
        Ok(key)
    }

    /// Load `singer`'s key.
    ///
    /// # Errors
    /// No key, or not 32 bytes.
    pub fn load(&self, singer: &str) -> Result<SigningKey, IdentityError> {
        let path = self.path(singer);
        let bytes = fs::read(&path).map_err(io(&path))?;
        let secret: [u8; 32] = bytes
            .try_into()
            .map_err(|_| IdentityError::Bad(path.clone(), "not a 32-byte key".into()))?;
        Ok(SigningKey::from_bytes(&secret))
    }
}

/// The roster file: the document and the owner's signature over its canonical bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedRoster {
    roster: RosterDoc,
    #[serde(with = "runo_core::types::serde_bytes_64")]
    sig: [u8; 64],
}

/// A chorus directory: the public files every host shares.
#[derive(Clone, Debug)]
pub struct ChorusDir(PathBuf);

impl ChorusDir {
    /// The chorus directory at `dir`.
    #[must_use]
    pub fn at(dir: &Path) -> Self {
        Self(dir.to_owned())
    }

    /// The chorus salt, created on first use.
    ///
    /// # Errors
    /// The file cannot be read or written, or is not 32 bytes.
    pub fn salt(&self) -> Result<[u8; 32], IdentityError> {
        let path = self.0.join("salt");
        if !path.exists() {
            fs::create_dir_all(&self.0).map_err(io(&self.0))?;
            let mut salt = [0u8; 32];
            rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut salt);
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(io(&path))?;
            f.write_all(&salt).map_err(io(&path))?;
            f.sync_all().map_err(io(&path))?;
        }
        let bytes = fs::read(&path).map_err(io(&path))?;
        bytes
            .try_into()
            .map_err(|_| IdentityError::Bad(path, "not 32 bytes".into()))
    }

    /// Write an enrolment for `runo init` to pick up.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn write_enrolment(&self, e: &Enrolment) -> Result<PathBuf, IdentityError> {
        let dir = self.0.join("enroll");
        fs::create_dir_all(&dir).map_err(io(&dir))?;
        let path = dir.join(format!("{}.cbor", e.singer.0));
        let bytes =
            canonical(e).map_err(|err| IdentityError::Bad(path.clone(), err.to_string()))?;
        write_atomic(&path, &bytes)?;
        Ok(path)
    }

    /// Every enrolment in the directory.
    ///
    /// # Errors
    /// A file cannot be read or decoded.
    pub fn enrolments(&self) -> Result<Vec<Enrolment>, IdentityError> {
        let dir = self.0.join("enroll");
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for path in paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|x| x == "cbor"))
        {
            let bytes = fs::read(&path).map_err(io(&path))?;
            out.push(decode(&bytes).map_err(|e| IdentityError::Bad(path.clone(), e.to_string()))?);
        }
        Ok(out)
    }

    /// Sign roster version 1 with the owner's key and write it.
    ///
    /// # Errors
    /// The key is not the owner's in the roster, or the file cannot be written.
    pub fn write_roster(
        &self,
        roster: &RosterDoc,
        owner: &SigningKey,
    ) -> Result<PathBuf, IdentityError> {
        let path = self.0.join(format!("roster.v{}.cbor", roster.version.0));
        let sig = sign_roster(owner, roster)
            .map_err(|e| IdentityError::Bad(path.clone(), e.to_string()))?;
        verify_roster(roster, &sig).map_err(|_| {
            IdentityError::Untrusted("the signing key is not the owner's key in this roster".into())
        })?;
        let file = SignedRoster {
            roster: roster.clone(),
            sig,
        };
        let bytes =
            canonical(&file).map_err(|e| IdentityError::Bad(path.clone(), e.to_string()))?;
        write_atomic(&path, &bytes)?;
        Ok(path)
    }

    /// Read and verify roster version `v`. If `pinned_owner` is given, the owner's key in the
    /// roster must be that key: version 1 is self-certifying, so a host pins the owner's key it
    /// was given out of band.
    ///
    /// # Errors
    /// The file is missing or malformed, the signature does not verify, or the owner differs.
    pub fn read_roster(
        &self,
        v: u32,
        pinned_owner: Option<&[u8; 32]>,
    ) -> Result<RosterDoc, IdentityError> {
        let path = self.0.join(format!("roster.v{v}.cbor"));
        let bytes = fs::read(&path).map_err(io(&path))?;
        let file: SignedRoster =
            decode(&bytes).map_err(|e| IdentityError::Bad(path.clone(), e.to_string()))?;
        verify_roster(&file.roster, &file.sig).map_err(|_| {
            IdentityError::Untrusted(format!("{} is not signed by its owner", path.display()))
        })?;
        if let Some(pin) = pinned_owner {
            let owner = file
                .roster
                .voices
                .iter()
                .find(|x| x.singer == file.roster.owner);
            if owner.map(|o| &o.key) != Some(pin) {
                return Err(IdentityError::Untrusted(
                    "the roster's owner is not the pinned owner key".into(),
                ));
            }
        }
        Ok(file.roster)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), IdentityError> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp).map_err(io(&tmp))?;
    f.write_all(bytes).map_err(io(&tmp))?;
    f.sync_all().map_err(io(&tmp))?;
    fs::rename(&tmp, path).map_err(io(path))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{ChorusDir, IdentityError, KeyHome};
    use runo_profile::manifest::{Enrolment, load_manifest, roster};

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("runo-identity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_key_is_made_once_private_and_loads_back() {
        let d = tmp("key");
        let home = KeyHome::at(&d);
        let k = home.generate("thinkpad").unwrap();
        assert!(matches!(
            home.generate("thinkpad"),
            Err(IdentityError::KeyExists(..))
        ));
        assert_eq!(home.load("thinkpad").unwrap().to_bytes(), k.to_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.join("keys/thinkpad.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn enrol_then_sign_and_read_the_roster_under_a_pinned_owner() {
        let d = tmp("roster");
        let home = KeyHome::at(&d.join("home"));
        let chorus = ChorusDir::at(&d.join("chorus"));
        let salt = chorus.salt().unwrap();
        assert_eq!(chorus.salt().unwrap(), salt, "the salt is made once");
        let m = load_manifest(include_str!("../../../examples/goliath/chorus.toml")).unwrap();
        for v in &m.voices {
            let k = home.generate(&v.singer).unwrap();
            chorus
                .write_enrolment(&Enrolment::new(
                    &v.singer,
                    k.verifying_key().to_bytes(),
                    &salt,
                    &v.credential,
                ))
                .unwrap();
        }
        let enrolled = chorus.enrolments().unwrap();
        assert_eq!(enrolled.len(), 4);
        let profiles: BTreeMap<String, runo_profile::Profile> = [
            (
                "../../profiles/goliath.toml",
                include_str!("../../../profiles/goliath.toml"),
            ),
            (
                "../../profiles/sentinel.toml",
                include_str!("../../../profiles/sentinel.toml"),
            ),
            (
                "../../profiles/owner.toml",
                include_str!("../../../profiles/owner.toml"),
            ),
        ]
        .into_iter()
        .map(|(p, t)| (p.to_owned(), runo_profile::load_profile(t).unwrap()))
        .collect();
        let doc = roster(&m, &profiles, &enrolled).unwrap();

        let desk = home.load("desk").unwrap();
        assert!(
            matches!(
                chorus.write_roster(&doc, &desk),
                Err(IdentityError::Untrusted(_))
            ),
            "only the owner signs"
        );
        let owner = home.load("owner").unwrap();
        chorus.write_roster(&doc, &owner).unwrap();
        let pin = owner.verifying_key().to_bytes();
        assert_eq!(chorus.read_roster(1, Some(&pin)).unwrap(), doc);
        let wrong = desk.verifying_key().to_bytes();
        assert!(matches!(
            chorus.read_roster(1, Some(&wrong)),
            Err(IdentityError::Untrusted(_))
        ));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
