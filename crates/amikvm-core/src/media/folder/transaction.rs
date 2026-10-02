//! Same-filesystem writeback. Public paths are removed only after ownership checks.
use super::{
    MAX_DEPTH, MAX_ENTRIES, atomic_json, cancelled, invalid, name_valid, remove_entry, stream_copy,
};
use crate::Result;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    volume: u64,
    file: [u8; 16],
}

fn identity(path: &Path) -> Result<Identity> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !(metadata.is_dir() || metadata.is_file()) {
        return Err(invalid(format!(
            "恢复路径不是普通文件或目录：{}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut file = [0; 16];
        file[..8].copy_from_slice(&metadata.ino().to_le_bytes());
        Ok(Identity {
            volume: metadata.dev(),
            file,
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_ID_INFO, FILE_READ_ATTRIBUTES, FileIdInfo, GetFileInformationByHandleEx,
        };
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid("恢复路径不能是重解析点"));
        }
        let handle = OpenOptions::new()
            .read(true)
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let mut info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
        if unsafe {
            GetFileInformationByHandleEx(
                handle.as_raw_handle(),
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Identity {
            volume: info.VolumeSerialNumber,
            file: info.FileId.Identifier,
        })
    }
}

/// Checking exists() before std::fs::rename is not sufficient: another writer
/// can create the destination between those two operations.
fn rename_new(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE)
            .map_err(std::io::Error::from)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let wide = |p: &Path| -> Result<Vec<u16>> {
            let mut v: Vec<_> = p.as_os_str().encode_wide().collect();
            if v.contains(&0) {
                return Err(invalid("文件路径包含 NUL"));
            }
            v.push(0);
            Ok(v)
        };
        let source = wide(source)?;
        let destination = wide(destination)?;
        if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

fn present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    identity: Identity,
    fingerprint: String,
}

/// Includes names, bytes, modification times and native permissions. A change
/// made to an installed directory after a crash must never be silently erased.
fn state(path: &Path, cancel: &AtomicBool) -> Result<State> {
    fn walk(
        path: &Path,
        relative: &str,
        depth: usize,
        count: &mut usize,
        hash: &mut Sha256,
        cancel: &AtomicBool,
    ) -> Result<()> {
        cancelled(cancel)?;
        *count += 1;
        if *count > MAX_ENTRIES || depth > MAX_DEPTH {
            return Err(invalid("恢复目录超过大小或层级限制"));
        }
        identity(path)?;
        let metadata = fs::symlink_metadata(path)?;
        let time = filetime::FileTime::from_last_modification_time(&metadata);
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::MetadataExt;
            (metadata.mode(), metadata.uid(), metadata.gid())
        };
        #[cfg(windows)]
        let permissions = {
            use std::os::windows::fs::MetadataExt;
            (metadata.file_attributes(), 0u32, 0u32)
        };
        hash.update(serde_json::to_vec(&(
            relative,
            metadata.is_dir(),
            time.unix_seconds(),
            time.nanoseconds(),
            permissions,
        ))?);
        if metadata.is_file() {
            let (bytes, content) =
                stream_copy(&mut File::open(path)?, &mut std::io::sink(), cancel)?;
            hash.update(bytes.to_le_bytes());
            hash.update(content.as_bytes());
            let after = fs::symlink_metadata(path)?;
            if bytes != metadata.len()
                || bytes != after.len()
                || filetime::FileTime::from_last_modification_time(&after) != time
            {
                return Err(invalid(format!(
                    "核对恢复内容时文件发生变化：{}",
                    path.display()
                )));
            }
        } else {
            let mut children = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            children.sort_by_key(|e| e.file_name());
            for entry in children {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| invalid("恢复目录包含非 Unicode 名称"))?;
                name_valid(&name)?;
                let child = if relative.is_empty() {
                    name
                } else {
                    format!("{relative}/{name}")
                };
                walk(&entry.path(), &child, depth + 1, count, hash, cancel)?;
            }
        }
        Ok(())
    }
    let before = identity(path)?;
    let mut hash = Sha256::new();
    walk(path, "", 0, &mut 0, &mut hash, cancel)?;
    if identity(path)? != before {
        return Err(invalid("核对恢复内容时路径被替换"));
    }
    Ok(State {
        identity: before,
        fingerprint: format!("{:x}", hash.finalize()),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Preparing,
    Installing,
    Committed,
    Restored,
    Cleaning,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    root: PathBuf,
    root_identity: Identity,
    backup: PathBuf,
    backup_identity: Identity,
    original_directory: Identity,
    prepared_directory: Identity,
    original: BTreeMap<String, State>,
    destinations: Vec<String>,
    prepared: BTreeMap<String, State>,
    phase: Phase,
}

pub(super) struct Transaction {
    journal_path: PathBuf,
    journal: Journal,
}

fn journal_path(image: &Path) -> PathBuf {
    let mut path = image.as_os_str().to_os_string();
    path.push(".amikvm-writeback.json");
    PathBuf::from(path)
}

impl Transaction {
    pub fn begin(
        root: &Path,
        image: &Path,
        originals: &BTreeSet<String>,
        destinations: &BTreeSet<String>,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        if present(&journal_path(image))? {
            return Err(invalid("存在尚未恢复的文件夹同步，请先重新预览"));
        }
        let backup = tempfile::Builder::new()
            .prefix(".amikvm-folder-backup-")
            .tempdir_in(root)?;
        fs::create_dir(backup.path().join("original"))?;
        fs::create_dir(backup.path().join("prepared"))?;
        let mut original = BTreeMap::new();
        for name in originals {
            original.insert(name.clone(), state(&root.join(name), cancel)?);
        }
        let journal = Journal {
            version: 2,
            root: root.to_owned(),
            root_identity: identity(root)?,
            backup: backup.path().to_owned(),
            backup_identity: identity(backup.path())?,
            original_directory: identity(&backup.path().join("original"))?,
            prepared_directory: identity(&backup.path().join("prepared"))?,
            original,
            destinations: destinations.iter().cloned().collect(),
            prepared: BTreeMap::new(),
            phase: Phase::Preparing,
        };
        let transaction = Self {
            journal_path: journal_path(image),
            journal,
        };
        atomic_json(&transaction.journal_path, &transaction.journal)?;
        let _ = backup.keep(); // Dropping the job cannot erase originals after journaling.
        Ok(transaction)
    }

    pub fn backup(&self) -> &Path {
        &self.journal.backup
    }
    pub fn prepared(&self) -> PathBuf {
        self.journal.backup.join("prepared")
    }
    fn originals(&self) -> PathBuf {
        self.journal.backup.join("original")
    }

    fn transition(&mut self, phase: Phase) -> Result<()> {
        let before = self.journal.phase;
        self.journal.phase = phase;
        if let Err(e) = atomic_json(&self.journal_path, &self.journal) {
            self.journal.phase = before;
            return Err(e);
        }
        Ok(())
    }

    pub fn ready(&mut self, cancel: &AtomicBool) -> Result<()> {
        for name in &self.journal.destinations {
            self.journal
                .prepared
                .insert(name.clone(), state(&self.prepared().join(name), cancel)?);
        }
        self.transition(Phase::Installing)
    }

    pub fn install(&self, cancel: &AtomicBool, progress: impl Fn(usize, bool)) -> Result<()> {
        for (index, (name, expected)) in self.journal.original.iter().enumerate() {
            cancelled(cancel)?;
            let source = self.journal.root.join(name);
            if state(&source, cancel)? != *expected {
                return Err(self.conflict(name));
            }
            rename_new(&source, &self.originals().join(name))?;
            if state(&self.originals().join(name), cancel)? != *expected {
                return Err(self.conflict(name));
            }
            progress(index + 1, true);
        }
        for (index, (name, expected)) in self.journal.prepared.iter().enumerate() {
            cancelled(cancel)?;
            let destination = self.journal.root.join(name);
            rename_new(&self.prepared().join(name), &destination)?;
            if state(&destination, cancel)? != *expected {
                return Err(self.conflict(name));
            }
            progress(index + 1, false);
        }
        cancelled(cancel)
    }

    pub fn commit(&mut self, cancel: &AtomicBool) -> Result<()> {
        for (name, expected) in &self.journal.prepared {
            if state(&self.journal.root.join(name), cancel)? != *expected {
                return Err(self.conflict(name));
            }
        }
        for (name, expected) in &self.journal.original {
            if state(&self.originals().join(name), cancel)? != *expected {
                return Err(self.conflict(name));
            }
        }
        cancelled(cancel)?;
        self.transition(Phase::Committed)?;
        self.finish()
    }

    fn conflict(&self, name: &str) -> crate::Error {
        invalid(format!(
            "文件夹恢复冲突：{name} 已被其他操作创建、替换或修改。原文件及同步内容已保留；请检查 {} 后重新预览",
            self.journal.backup.display()
        ))
    }

    fn validate(&self) -> Result<bool> {
        let j = &self.journal;
        if j.version != 2
            || identity(&j.root)? != j.root_identity
            || j.backup.parent() != Some(j.root.as_path())
            || !j
                .backup
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".amikvm-folder-backup-"))
        {
            return Err(invalid(
                "文件夹恢复日志的路径或身份无效，请保留工作镜像与备份",
            ));
        }
        for names in [
            j.original.keys().cloned().collect::<Vec<_>>(),
            j.destinations.clone(),
        ] {
            let mut unique = BTreeSet::new();
            for name in names {
                name_valid(&name)?;
                if !unique.insert(name.to_uppercase()) {
                    return Err(invalid("文件夹恢复日志含重复路径"));
                }
            }
        }
        let destinations: BTreeSet<_> = j.destinations.iter().collect();
        if j.prepared.keys().any(|name| !destinations.contains(name))
            || (j.phase == Phase::Installing && j.prepared.len() != destinations.len())
        {
            return Err(invalid("文件夹恢复日志含未知目标"));
        }
        if !present(&j.backup)? {
            return Ok(false);
        }
        if identity(&j.backup)? != j.backup_identity || !fs::symlink_metadata(&j.backup)?.is_dir() {
            return Err(invalid("文件夹备份目录被替换"));
        }
        for (name, expected) in [
            ("original", &j.original_directory),
            ("prepared", &j.prepared_directory),
        ] {
            let path = j.backup.join(name);
            if present(&path)? {
                if identity(&path)? != *expected || !fs::symlink_metadata(&path)?.is_dir() {
                    return Err(invalid("文件夹备份子目录被替换"));
                }
            } else if j.phase != Phase::Cleaning {
                return Err(invalid("文件夹备份子目录丢失"));
            }
        }
        if fs::read_dir(&j.backup)?.any(|e| {
            e.map_or(true, |e| {
                !["original", "prepared"].iter().any(|n| e.file_name() == *n)
            })
        }) {
            return Err(invalid("备份目录包含未识别的文件，已保留备份"));
        }
        Ok(true)
    }

    pub fn rollback(&mut self) -> Result<()> {
        let backup_exists = self.validate()?;
        if [Phase::Committed, Phase::Restored, Phase::Cleaning].contains(&self.journal.phase) {
            return self.finish();
        }
        if !backup_exists && self.journal.phase == Phase::Installing {
            return Err(invalid("中断同步的备份目录丢失，请保留工作镜像"));
        }
        let cancel = AtomicBool::new(false);
        if self.journal.phase == Phase::Installing {
            for (name, expected) in &self.journal.prepared {
                let prepared = self.prepared().join(name);
                let public = self.journal.root.join(name);
                if present(&prepared)? {
                    if state(&prepared, &cancel)? != *expected {
                        return Err(self.conflict(name));
                    }
                    // Still staged or already moved back. A public namesake is not ours.
                    continue;
                }
                if !present(&public)? {
                    continue;
                }
                let current = state(&public, &cancel)?;
                if current != *expected {
                    let restored = self.journal.original.iter().any(|(old, original)| {
                        old.to_uppercase() == name.to_uppercase()
                            && current.identity == original.identity
                    });
                    if restored {
                        continue;
                    }
                    return Err(self.conflict(name));
                }
                rename_new(&public, &prepared)?;
                if state(&prepared, &cancel)? != *expected {
                    // A writer retained a handle across the rename. Put its changes back
                    // if possible; even on a second conflict, no file is deleted.
                    let _ = rename_new(&prepared, &public);
                    return Err(self.conflict(name));
                }
            }
            for (name, original) in &self.journal.original {
                let saved = self.originals().join(name);
                let public = self.journal.root.join(name);
                if present(&saved)? {
                    if identity(&saved)? != original.identity {
                        return Err(self.conflict(name));
                    }
                    // Restore the original object, including any edits through an open
                    // handle while it was in backup. Never overwrite a new public path.
                    rename_new(&saved, &public).map_err(|_| self.conflict(name))?;
                } else if !present(&public)? || identity(&public)? != original.identity {
                    return Err(self.conflict(name));
                }
            }
        }
        self.transition(Phase::Restored)?;
        self.finish()
    }

    fn finish(&mut self) -> Result<()> {
        if self.journal.phase == Phase::Committed {
            // An interrupted commit may have left backups that were subsequently edited.
            // Preserve those edits rather than treating all private files as disposable.
            let cancel = AtomicBool::new(false);
            for (name, expected) in &self.journal.original {
                let saved = self.originals().join(name);
                if present(&saved)? && state(&saved, &cancel)? != *expected {
                    return Err(self.conflict(name));
                }
            }
        }
        self.validate()?;
        if self.journal.phase != Phase::Cleaning {
            self.transition(Phase::Cleaning)?;
        }
        // Cleaning is written before deletion. A second recovery after partial cleanup
        // cannot restore originals over successfully installed or restored public files.
        remove_entry(&self.journal.backup)?;
        fs::remove_file(&self.journal_path)?;
        Ok(())
    }
}

pub(super) fn recover(root: &Path, image: &Path) -> Result<()> {
    let journal_path = journal_path(image);
    let journal: Journal = match fs::read(&journal_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| invalid("无法确认文件夹恢复日志，请保留镜像及备份后检查"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if journal.root != root {
        return Err(invalid("文件夹恢复日志与源目录不符"));
    }
    let _lock = match OpenOptions::new().read(true).write(true).open(image) {
        Ok(file) => {
            file.try_lock_exclusive()
                .map_err(|_| invalid("工作镜像仍在使用，不能恢复文件夹"))?;
            Some(file)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    Transaction {
        journal_path,
        journal,
    }
    .rollback()
}
