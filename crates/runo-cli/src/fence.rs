//! `runo fence relaunch` and `runo fence close`: the allocator's commands (technical spec §8,
//! §11). They run on the allocator's host only, because they sign with the allocator's key.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use runo_core::hash::display;
use runo_core::{Payload, SingerId, TaskId, Timestamp};
use runo_fence::journal::JsonlJournal;
use runo_fence::{Allocator, Outcome};
use runo_node::identity::KeyHome;

/// Where the allocator's journal lives unless given: `$XDG_STATE_HOME/runo/allocator.jsonl`.
pub fn journal_path(given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| {
        let base = std::env::var_os("XDG_STATE_HOME").map_or_else(
            || PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"),
            PathBuf::from,
        );
        base.join("runo/allocator.jsonl")
    })
}

/// What the allocator commands share.
pub struct FenceArgs<'a> {
    /// The chorus directory holding the signed roster.
    pub chorus: &'a Path,
    /// The store.
    pub store: &'a str,
    /// The owner's pinned key, hex.
    pub owner_key: Option<&'a str>,
    /// The journal.
    pub journal: PathBuf,
}

fn allocator(
    a: &FenceArgs<'_>,
    rt: &tokio::runtime::Runtime,
) -> Result<Allocator<JsonlJournal>, String> {
    let roster = crate::node::roster(a.chorus, a.owner_key)?;
    let key = KeyHome::from_env()
        .load(&roster.allocator.0)
        .map_err(|e| format!("{e} (the fence commands run on the allocator's host)"))?;
    let journal = JsonlJournal::open(&a.journal).map_err(|e| e.to_string())?;
    let store = rt.block_on(crate::node::connect(a.store))?;
    Allocator::new(
        key,
        roster,
        Arc::new(store),
        journal,
        Box::new(|| Timestamp(runo_core::time::wall::format(runo_core::time::wall::now()))),
    )
    .map_err(|e| e.to_string())
}

/// `runo fence relaunch --singer --task`.
pub fn relaunch(a: &FenceArgs<'_>, singer: &str, task: &str) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let al = allocator(a, &rt)?;
    let (s, t) = (SingerId(singer.into()), TaskId(task.into()));
    match rt
        .block_on(al.relaunch(&s, &t))
        .map_err(|e| e.to_string())?
    {
        Outcome::Activated { grant, activation } => {
            println!("relaunched {singer} on {task}");
            println!("  grant       {}", display(&grant.0));
            println!("  activation  {}", display(&activation.0));
            println!("  start the replacement with --grant {}", display(&grant.0));
            Ok(())
        }
        Outcome::NotActivated { grant, missing } => Err(format!(
            "not activated: {} did not acknowledge grant {}. Nothing new was started; run the \
             same command again to retry with the same grant",
            missing
                .iter()
                .map(|r| r.0.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            display(&grant.0)
        )),
    }
}

/// `runo fence close --task`.
pub fn close(a: &FenceArgs<'_>, task: &str) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let al = allocator(a, &rt)?;
    let close = rt
        .block_on(al.close(&TaskId(task.into())))
        .map_err(|e| format!("{e} (closing resumes where it stopped: run it again)"))?;
    let Payload::Close { snapshot, .. } = &close.payload else {
        return Err("the allocator returned something other than Close".into());
    };
    println!("closed {task}");
    println!("  snapshot  {}", display(&snapshot.hash));
    println!("  close     {}", display(&close.seal.sum.0));
    Ok(())
}
