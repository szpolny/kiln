//! Local-only state file. The caller picks the path; nothing here knows about
//! the user's real data directory, so tests always use a temp dir.

use crate::store::{Status, Store};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    store: Store,
}

#[derive(Debug)]
pub enum Loaded {
    /// No state file yet.
    Missing,
    Ok(Store),
    /// File was unreadable; it was moved aside to the given name.
    Corrupt(std::path::PathBuf),
}

/// Write atomically: a crash mid-write never leaves a half file.
pub fn save(path: &Path, store: &Store) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(&File { version: VERSION, store: store.clone() })
        .map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

pub fn load(path: &Path) -> io::Result<Loaded> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Loaded::Missing),
        Err(e) => return Err(e),
    };
    match serde_json::from_slice::<File>(&bytes) {
        Ok(f) if f.version == VERSION => {
            let mut store = f.store;
            store.normalize();
            // Child processes do not survive a restart.
            for t in store.projects.iter_mut().flat_map(|p| &mut p.threads) {
                if t.status == Status::Running {
                    t.status = Status::Idle;
                }
            }
            Ok(Loaded::Ok(store))
        }
        _ => {
            let aside = path.with_extension("json.corrupt");
            std::fs::rename(path, &aside)?;
            Ok(Loaded::Corrupt(aside))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Approval;

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("kiln-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("state.json")
    }

    #[test]
    fn round_trips_sessions_approvals_and_policy() {
        let path = temp("roundtrip");
        let mut store = Store::sample();
        let id = store.projects[0].threads[0].id;
        let t = store.thread_mut(id).unwrap();
        t.session_id = Some("sess-1".into());
        t.model = Some("opus".into());
        t.pending.push(Approval {
            tool: "Bash".into(),
            summary: "touch a".into(),
            rule: "Bash(touch a)".into(),
        });
        t.policy.network = crate::policy::Decision::Allow;
        save(&path, &store).unwrap();

        let Loaded::Ok(back) = load(&path).unwrap() else { panic!("expected state") };
        let t = back.thread(id).unwrap();
        assert_eq!(t.session_id.as_deref(), Some("sess-1"));
        assert_eq!(t.model.as_deref(), Some("opus"));
        assert_eq!(t.pending.len(), 1);
        assert!(!t.policy.is_local_only());
            }

    #[test]
    fn running_threads_restore_as_idle() {
        let path = temp("running");
        let mut store = Store::sample();
        let id = store.projects[0].threads[0].id;
        store.thread_mut(id).unwrap().status = Status::Running;
        save(&path, &store).unwrap();
        let Loaded::Ok(back) = load(&path).unwrap() else { panic!() };
        assert_eq!(back.thread(id).unwrap().status, Status::Idle);
    }

    #[test]
    fn missing_and_corrupt_files_are_handled() {
        let path = temp("bad");
        assert!(matches!(load(&path).unwrap(), Loaded::Missing));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let Loaded::Corrupt(aside) = load(&path).unwrap() else { panic!() };
        assert!(aside.exists() && !path.exists());
    }
}
