use crate::error::StoreError;
use crate::sqlite::SqliteStore;
use ours_types::GroupId;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

const EXTENSION: &str = "db";
const MAX_ID_LEN: usize = 128;

/// A directory of group databases, one `<group_id>.db` file per group.
/// Opening the same group twice returns the same store.
#[derive(Debug)]
pub struct StoreDir {
    dir: PathBuf,
    open: Mutex<HashMap<GroupId, Arc<SqliteStore>>>,
}

impl StoreDir {
    /// Opens the directory, creating it and any missing parents.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            open: Mutex::new(HashMap::new()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Where the group's database lives, whether or not it exists yet.
    pub fn group_path(&self, group: &GroupId) -> Result<PathBuf, StoreError> {
        check_id(group.as_str())?;
        Ok(self.dir.join(format!("{group}.{EXTENSION}")))
    }

    /// True when the group has a database file.
    pub fn exists(&self, group: &GroupId) -> Result<bool, StoreError> {
        Ok(self.group_path(group)?.is_file())
    }

    /// The group's store, created on first use.
    pub fn open_group(&self, group: &GroupId) -> Result<Arc<SqliteStore>, StoreError> {
        let path = self.group_path(group)?;
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(store) = open.get(group) {
            return Ok(Arc::clone(store));
        }
        let store = Arc::new(SqliteStore::open(path)?);
        open.insert(group.clone(), Arc::clone(&store));
        Ok(store)
    }

    /// Forgets the cached store so the file can be moved or removed once
    /// every outstanding handle is dropped.
    pub fn close_group(&self, group: &GroupId) {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(group);
    }

    /// Deletes the group's database with its `-wal` and `-shm` files; missing
    /// files are fine. The caller must have dropped every handle it holds,
    /// since the cached store is released here but other `Arc`s stay open.
    pub fn wipe_group(&self, group: &GroupId) -> Result<(), StoreError> {
        let path = self.group_path(group)?;
        self.close_group(group);
        for suffix in ["", "-wal", "-shm"] {
            let mut name = path.clone().into_os_string();
            name.push(suffix);
            match std::fs::remove_file(PathBuf::from(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Every group with a database file, sorted by id.
    pub fn groups(&self) -> Result<Vec<GroupId>, StoreError> {
        let mut groups = BTreeSet::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if check_id(stem).is_ok() {
                groups.insert(GroupId::new(stem));
            }
        }
        Ok(groups.into_iter().collect())
    }
}

/// Ids become file names, so only characters that cannot climb out of the
/// directory are allowed. ULIDs always pass.
fn check_id(id: &str) -> Result<(), StoreError> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(StoreError::InvalidGroupId(id.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::EventLog;
    use ours_types::Instant;

    fn now() -> Instant {
        Instant::from_millis(1_793_613_600_000).unwrap()
    }

    #[test]
    fn creates_a_missing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b");
        let dir = StoreDir::open(&nested).unwrap();
        assert!(nested.is_dir());
        assert_eq!(dir.path(), nested);
        assert!(dir.groups().unwrap().is_empty());
    }

    #[test]
    fn lists_groups_after_creating_two() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = StoreDir::open(tmp.path()).unwrap();
        let a = GroupId::generate();
        let b = GroupId::generate();
        assert!(!dir.exists(&a).unwrap());
        dir.open_group(&b)
            .unwrap()
            .append("msg", b"x", now())
            .unwrap();
        dir.open_group(&a)
            .unwrap()
            .append("msg", b"y", now())
            .unwrap();
        assert!(dir.exists(&a).unwrap());

        let mut want = vec![a.clone(), b.clone()];
        want.sort();
        assert_eq!(dir.groups().unwrap(), want);
        assert_eq!(
            dir.group_path(&a).unwrap(),
            tmp.path().join(format!("{a}.db"))
        );
    }

    #[test]
    fn listing_ignores_sidecars_and_strangers() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = StoreDir::open(tmp.path()).unwrap();
        let group = GroupId::new("g1");
        dir.open_group(&group)
            .unwrap()
            .append("msg", b"x", now())
            .unwrap();
        std::fs::write(tmp.path().join("notes.txt"), b"").unwrap();
        std::fs::write(tmp.path().join("bad name.db"), b"").unwrap();
        std::fs::create_dir(tmp.path().join("subdir.db")).unwrap();
        assert_eq!(dir.groups().unwrap(), vec![group]);
    }

    #[test]
    fn groups_do_not_share_data() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = StoreDir::open(tmp.path()).unwrap();
        let a = dir.open_group(&GroupId::new("a")).unwrap();
        let b = dir.open_group(&GroupId::new("b")).unwrap();
        a.append("msg", b"x", now()).unwrap();
        a.set("k", b"v").unwrap();
        assert_eq!(b.latest().unwrap(), None);
        assert_eq!(b.get("k").unwrap(), None);
    }

    #[test]
    fn the_same_group_gets_the_same_store() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = StoreDir::open(tmp.path()).unwrap();
        let group = GroupId::new("g");
        let first = dir.open_group(&group).unwrap();
        let second = dir.open_group(&group).unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        dir.close_group(&group);
        let third = dir.open_group(&group).unwrap();
        assert!(!Arc::ptr_eq(&first, &third));
    }

    #[test]
    fn wipe_removes_the_files_and_tolerates_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = StoreDir::open(tmp.path()).unwrap();
        let group = GroupId::new("g");
        dir.wipe_group(&group).unwrap();

        let store = dir.open_group(&group).unwrap();
        store.append("msg", b"x", now()).unwrap();
        drop(store);
        std::fs::write(tmp.path().join("g.db-wal"), b"w").unwrap();
        std::fs::write(tmp.path().join("g.db-shm"), b"s").unwrap();
        dir.wipe_group(&group).unwrap();

        assert!(!dir.exists(&group).unwrap());
        assert!(!tmp.path().join("g.db-wal").exists());
        assert!(!tmp.path().join("g.db-shm").exists());
        assert!(dir.groups().unwrap().is_empty());
        let fresh = dir.open_group(&group).unwrap();
        assert_eq!(fresh.latest().unwrap(), None);
    }

    #[test]
    fn data_survives_reopening_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let group = GroupId::generate();
        let written = {
            let dir = StoreDir::open(tmp.path()).unwrap();
            dir.open_group(&group)
                .unwrap()
                .append("msg", b"x", now())
                .unwrap()
        };
        let dir = StoreDir::open(tmp.path()).unwrap();
        let store = dir.open_group(&group).unwrap();
        assert_eq!(store.latest().unwrap().unwrap(), written);
        assert_eq!(store.verify_chain().unwrap(), 1);
    }

    #[test]
    fn ids_that_could_escape_the_directory_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = StoreDir::open(tmp.path()).unwrap();
        for id in [
            "",
            "..",
            "../x",
            "a/b",
            "a\\b",
            "a.b",
            "a b",
            &"x".repeat(129),
        ] {
            assert!(matches!(
                dir.open_group(&GroupId::new(id)),
                Err(StoreError::InvalidGroupId(_))
            ));
        }
        assert!(dir.open_group(&GroupId::new("Group_1-b")).is_ok());
    }
}
