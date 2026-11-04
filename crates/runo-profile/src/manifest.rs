//! The chorus manifest `runo init` reads, its load-time checks, and roster version 1
//! (technical spec §10). Pure: the caller reads the files and hands in their text.

use std::collections::{BTreeMap, BTreeSet};

use runo_core::hash::fingerprint;
use runo_core::{Domains, Hash, ResourceId, RosterDoc, RosterVersion, SingerId, VoiceEntry};
use serde::{Deserialize, Serialize};

use crate::{Profile, ProfileError};

/// Resources a fence can be installed on today. The song is the store; the others are the
/// consumers' named resources from the Song spec's profile table.
pub const KNOWN_RESOURCES: [&str; 4] = ["song", "state-vessel", "merge-gate", "worktree"];

/// `chorus.toml`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The chorus's name; also the run name of roster version 1.
    pub name: String,
    /// The singer that allocates fences (a voice in this manifest).
    pub allocator: String,
    /// Retention, unused by Core checks.
    #[serde(default)]
    pub retention: Option<toml::Value>,
    /// The voices.
    #[serde(rename = "voice")]
    pub voices: Vec<Voice>,
}

/// One `[[voice]]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Voice {
    /// Its identity.
    pub singer: String,
    /// Path to its profile, relative to the manifest.
    pub profile: String,
    /// The host it runs on.
    pub host: String,
    /// Its model provider, or `none`.
    pub provider: String,
    /// A reference to its credential (`file://…`, `bao://…`, `token://…`); never the secret.
    pub credential: String,
    /// Its network path.
    pub network: String,
}

/// What `runo enroll` writes for `runo init`: a voice's public key and its credential
/// fingerprint. The raw credential reference stays on the voice's host.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enrolment {
    /// The voice.
    pub singer: SingerId,
    /// Its Ed25519 verifying key.
    pub key: [u8; 32],
    /// `blake3::keyed_hash(chorus_salt, credential reference)`.
    pub credential: Hash,
}

impl Enrolment {
    /// An enrolment for `singer` with `key`, fingerprinting `credential` under the chorus salt.
    #[must_use]
    pub fn new(singer: &str, key: [u8; 32], salt: &[u8; 32], credential: &str) -> Self {
        Self {
            singer: SingerId(singer.to_owned()),
            key,
            credential: fingerprint(salt, credential),
        }
    }
}

/// Parse `chorus.toml`.
///
/// # Errors
/// The text is not a manifest.
pub fn load_manifest(toml_text: &str) -> Result<Manifest, ProfileError> {
    toml::from_str(toml_text).map_err(|e| ProfileError::Parse(e.to_string()))
}

fn check(ok: bool, msg: impl Into<String>) -> Result<(), ProfileError> {
    if ok {
        Ok(())
    } else {
        Err(ProfileError::Check(msg.into()))
    }
}

/// The load-time checks of technical spec §10, given each voice's loaded profile (keyed by the
/// manifest's `profile` path). Profile-level checks already ran in `load_profile`.
///
/// # Errors
/// The first check that fails, naming what to change.
pub fn check_chorus(
    m: &Manifest,
    profiles: &BTreeMap<String, Profile>,
) -> Result<(), ProfileError> {
    let mut singers = BTreeSet::new();
    for v in &m.voices {
        check(
            singers.insert(v.singer.as_str()),
            format!("voice {} is listed twice", v.singer),
        )?;
        check(
            profiles.contains_key(&v.profile),
            format!("voice {}: profile {} not loaded", v.singer, v.profile),
        )?;
    }
    let role = |v: &Voice| profiles[&v.profile].role.as_str();
    let machines: Vec<&Voice> = m.voices.iter().filter(|v| role(v) != "owner").collect();
    check(
        machines.len() >= 3,
        format!(
            "a chorus needs at least three voices with a non-human cadence; this one has {}",
            machines.len()
        ),
    )?;
    check(
        m.voices.iter().filter(|v| role(v) == "owner").count() == 1,
        "exactly one voice must have the owner profile",
    )?;
    check(
        singers.contains(m.allocator.as_str()),
        format!("allocator {} is not a voice", m.allocator),
    )?;
    let all_same = |f: fn(&Voice) -> &str| machines.windows(2).all(|w| f(w[0]) == f(w[1]));
    check(
        !all_same(|v| v.host.as_str()),
        "every voice is on one host: add a voice on another host",
    )?;
    check(
        !(all_same(|v| v.credential.as_str()) && all_same(|v| v.provider.as_str())),
        "every voice shares one credential and provider: at least one voice must be outside the others' domains",
    )?;
    for v in &m.voices {
        let p = &profiles[&v.profile];
        for r in &p.fenced.resources {
            check(
                KNOWN_RESOURCES.contains(&r.as_str()),
                format!(
                    "voice {}: no allocator can install a fence on {r}",
                    v.singer
                ),
            )?;
        }
        let d = p.durability.as_str();
        check(
            matches!(d, "process" | "host")
                || d.strip_prefix("replicated:")
                    .is_some_and(|k| k.parse::<u8>().is_ok()),
            format!(
                "voice {}: durability {d} is not process, host or replicated:<k>",
                v.singer
            ),
        )?;
    }
    Ok(())
}

/// Roster version 1 from a checked manifest and one enrolment per voice.
///
/// # Errors
/// A voice has no enrolment, or an enrolment names no voice.
pub fn roster(
    m: &Manifest,
    profiles: &BTreeMap<String, Profile>,
    enrolments: &[Enrolment],
) -> Result<RosterDoc, ProfileError> {
    let by_singer: BTreeMap<&str, &Enrolment> = enrolments
        .iter()
        .map(|e| (e.singer.0.as_str(), e))
        .collect();
    for e in enrolments {
        check(
            m.voices.iter().any(|v| v.singer == e.singer.0),
            format!(
                "enrolment for {} names no voice in the manifest",
                e.singer.0
            ),
        )?;
    }
    let mut voices = Vec::new();
    let mut owner = None;
    for v in &m.voices {
        let e = by_singer.get(v.singer.as_str()).ok_or_else(|| {
            ProfileError::Check(format!(
                "voice {} has not enrolled: run `runo enroll --singer {}`",
                v.singer, v.singer
            ))
        })?;
        let p = &profiles[&v.profile];
        if p.role == "owner" {
            owner = Some(SingerId(v.singer.clone()));
        }
        voices.push(VoiceEntry {
            singer: SingerId(v.singer.clone()),
            key: e.key,
            role: p.role.clone(),
            domains: Domains {
                host: v.host.clone(),
                credential: e.credential,
                provider: v.provider.clone(),
                network: v.network.clone(),
            },
            fenced: p
                .fenced
                .resources
                .iter()
                .map(|r| ResourceId(r.clone()))
                .collect(),
        });
    }
    Ok(RosterDoc {
        version: RosterVersion(1),
        voices,
        allocator: SingerId(m.allocator.clone()),
        owner: owner.ok_or_else(|| ProfileError::Check("no owner voice".into()))?,
        run: m.name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Enrolment, check_chorus, load_manifest, roster};
    use crate::{ProfileError, load_profile};

    const GOLIATH: &str = include_str!("../../../examples/goliath/chorus.toml");

    fn profiles() -> BTreeMap<String, crate::Profile> {
        [
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
        .map(|(p, t)| (p.to_owned(), load_profile(t).unwrap()))
        .collect()
    }

    fn enrolments(m: &super::Manifest) -> Vec<Enrolment> {
        m.voices
            .iter()
            .enumerate()
            .map(|(i, v)| {
                Enrolment::new(
                    &v.singer,
                    [u8::try_from(i).unwrap(); 32],
                    &[7; 32],
                    &v.credential,
                )
            })
            .collect()
    }

    #[test]
    fn the_goliath_example_checks_and_makes_roster_one() {
        let m = load_manifest(GOLIATH).unwrap();
        let p = profiles();
        check_chorus(&m, &p).unwrap();
        let r = roster(&m, &p, &enrolments(&m)).unwrap();
        assert_eq!(r.version.0, 1);
        assert_eq!(r.owner.0, "owner");
        assert_eq!(r.run, "goliath");
        assert_eq!(r.voices.len(), m.voices.len());
        let laptop = r.voices.iter().find(|v| v.singer.0 == "thinkpad").unwrap();
        assert_eq!(laptop.role, "machine");
        assert_eq!(laptop.fenced.len(), 2);
        assert_ne!(
            r.voices[0].domains.credential, r.voices[1].domains.credential,
            "fingerprints differ per reference"
        );
    }

    fn refused(m: &super::Manifest) -> String {
        match check_chorus(m, &profiles()) {
            Err(ProfileError::Check(why)) => why,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_load_time_checks_name_what_to_change() {
        let mut m = load_manifest(GOLIATH).unwrap();
        let two = load_manifest(GOLIATH).unwrap();
        m.voices.retain(|v| v.singer != "sentinel");
        assert!(
            refused(&m).contains("at least three voices"),
            "{}",
            refused(&m)
        );

        let mut one_host = two.clone();
        for v in &mut one_host.voices {
            "docker".clone_into(&mut v.host);
        }
        assert!(refused(&one_host).contains("one host"));

        let mut twice = two.clone();
        twice.voices.push(twice.voices[0].clone());
        assert!(refused(&twice).contains("listed twice"));

        let mut alloc = two;
        "nobody".clone_into(&mut alloc.allocator);
        assert!(refused(&alloc).contains("allocator nobody"));
    }

    #[test]
    fn every_voice_must_enroll() {
        let m = load_manifest(GOLIATH).unwrap();
        let mut e = enrolments(&m);
        e.retain(|e| e.singer.0 != "desk");
        let err = roster(&m, &profiles(), &e).unwrap_err().to_string();
        assert!(err.contains("voice desk has not enrolled"), "{err}");
    }
}
