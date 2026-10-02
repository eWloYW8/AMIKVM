//! Portable FAT folder redirection. All file decisions and writeback live in Rust.
//! A disconnected writable image is retained until the user applies or discards it.
use crate::{Error, Result};
use chrono::{Datelike, Local, TimeZone, Timelike};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tempfile::{NamedTempFile, TempDir};

mod transaction;

const MAX_ENTRIES: usize = 100_000;
const MAX_DEPTH: usize = 128;
type Tree = BTreeMap<String, Entry>;

// A cyclic FAT chain containing only deleted entries can loop inside DirIter::next,
// before the caller gets a chance to count entries or check cancellation.
struct BoundedDisk<'a> {
    file: &'a File,
    cancel: &'a AtomicBool,
    remaining_operations: u32,
    remaining_read: u64,
    length: u64,
}
impl BoundedDisk<'_> {
    fn check(&mut self) -> std::io::Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                // read_exact retries Interrupted; cancellation must propagate.
                std::io::ErrorKind::Other,
                "文件夹操作已取消",
            ));
        }
        if self.remaining_operations == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "FAT 镜像解析超过操作上限，可能存在循环目录链",
            ));
        }
        self.remaining_operations -= 1;
        Ok(())
    }
}
impl Read for BoundedDisk<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.check()?;
        if self.remaining_read < buffer.len() as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "FAT 镜像解析超过读取上限",
            ));
        }
        let n = self.file.read(buffer)?;
        self.remaining_read -= n as u64;
        Ok(n)
    }
}
impl Seek for BoundedDisk<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.check()?;
        let position = self.file.seek(position)?;
        if position > self.length {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "FAT 镜像包含越界地址",
            ));
        }
        Ok(position)
    }
}
impl Write for BoundedDisk<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.check()?;
        if self
            .file
            .stream_position()?
            .saturating_add(bytes.len() as u64)
            > self.length
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "FAT 元数据写入越界",
            ));
        }
        self.file.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.sync_data()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    directory: bool,
    size: u64,
    hash: String,
    modified_seconds: i64,
    modified_nanos: u32,
    readonly: bool,
}
impl Entry {
    fn content_eq(&self, other: &Self) -> bool {
        self.directory == other.directory
            && (self.directory || (self.size == other.size && self.hash == other.hash))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mapping {
    pub root: PathBuf,
    pub image: PathBuf,
    pub size_mib: u32,
    pub readonly: bool,
    baseline: Tree,
    #[serde(default)]
    image_baseline: Option<Tree>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Progress {
    pub stage: &'static str,
    pub files: usize,
    pub bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub path: String,
    pub operation: &'static str,
}

pub struct Plan {
    pub changes: Vec<Change>,
    pub conflicts: Vec<String>,
    local: Tree,
    remote: Tree,
    updates: Tree,
    remote_files: BTreeSet<String>,
    staged: TempDir,
    // Keep the image locked between preview and apply; a mounted image cannot be read back.
    _image_lock: File,
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut temp = NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer(&mut temp, value)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(invalid("文件夹操作已取消"))
    } else {
        Ok(())
    }
}
fn name_valid(name: &str) -> Result<()> {
    let stem = name.split('.').next().unwrap_or_default().to_uppercase();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.encode_utf16().count() > 255
        || name
            .chars()
            .any(|c| c.is_control() || "\"*/:<>?\\|".contains(c))
        || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(invalid(format!("FAT 或目标平台不支持这个文件名：{name}")));
    }
    Ok(())
}
fn join_key(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.into()
    } else {
        format!("{parent}/{name}")
    }
}
fn below(path: &str, parent: &str) -> bool {
    path == parent
        || path
            .strip_prefix(parent)
            .is_some_and(|s| s.starts_with('/'))
}
fn same(a: Option<&Entry>, b: Option<&Entry>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.content_eq(b),
        _ => false,
    }
}
fn same_state(a: Option<&Entry>, b: Option<&Entry>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.content_eq(b) && same_time(a, b) && a.readonly == b.readonly,
        _ => false,
    }
}
fn same_time(a: &Entry, b: &Entry) -> bool {
    (a.modified_seconds, a.modified_nanos) == (b.modified_seconds, b.modified_nanos)
}
fn fat_seconds(time: fatfs::DateTime) -> Result<i64> {
    Local
        .with_ymd_and_hms(
            time.date.year as i32,
            time.date.month as u32,
            time.date.day as u32,
            time.time.hour as u32,
            time.time.min as u32,
            (time.time.sec / 2 * 2) as u32,
        )
        .earliest()
        .map(|t| t.timestamp())
        .ok_or_else(|| invalid("FAT 目录项包含无效修改时间"))
}
fn image_entry(entry: &Entry) -> Result<Entry> {
    let mut expected = entry.clone();
    expected.modified_seconds = fat_seconds(fat_time(entry))?;
    expected.modified_nanos = 0;
    Ok(expected)
}
fn stream_copy(
    reader: &mut impl Read,
    writer: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<(u64, String)> {
    let mut buffer = [0u8; 65_536];
    let mut hasher = Sha256::new();
    let mut bytes = 0;
    loop {
        cancelled(cancel)?;
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buffer[..n])?;
        hasher.update(&buffer[..n]);
        bytes += n as u64;
    }
    Ok((bytes, format!("{:x}", hasher.finalize())))
}
fn scan(root: &Path, cancel: &AtomicBool) -> Result<Tree> {
    scan_excluding(root, cancel, None)
}
fn scan_excluding(root: &Path, cancel: &AtomicBool, exclude: Option<&Path>) -> Result<Tree> {
    fn walk(
        root: &Path,
        parent: &str,
        tree: &mut Tree,
        cancel: &AtomicBool,
        exclude: Option<&Path>,
    ) -> Result<()> {
        cancelled(cancel)?;
        if parent.split('/').count() > MAX_DEPTH {
            return Err(invalid("目录层级超过 128 层"));
        }
        let mut names = BTreeSet::new();
        for item in fs::read_dir(root.join(parent))? {
            cancelled(cancel)?;
            let item = item?;
            if exclude.is_some_and(|p| item.path() == p) {
                continue;
            }
            let name = item
                .file_name()
                .into_string()
                .map_err(|_| invalid("目录包含非 Unicode 文件名"))?;
            name_valid(&name)?;
            if !names.insert(name.to_uppercase()) {
                return Err(invalid(format!("FAT 文件名大小写冲突：{name}")));
            }
            let path = item.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !(metadata.is_dir() || metadata.is_file()) {
                return Err(invalid(format!(
                    "不支持符号链接或特殊文件：{}",
                    path.display()
                )));
            }
            let modified = filetime::FileTime::from_last_modification_time(&metadata);
            let mut entry = Entry {
                directory: metadata.is_dir(),
                size: 0,
                hash: String::new(),
                modified_seconds: modified.unix_seconds(),
                modified_nanos: modified.nanoseconds(),
                readonly: metadata.permissions().readonly(),
            };
            if metadata.is_file() {
                (entry.size, entry.hash) =
                    stream_copy(&mut File::open(&path)?, &mut std::io::sink(), cancel)?;
                let after = fs::metadata(&path)?;
                if metadata.len() != entry.size
                    || after.len() != entry.size
                    || filetime::FileTime::from_last_modification_time(&after) != modified
                {
                    return Err(invalid(format!("读取时文件发生变化：{}", path.display())));
                }
            }
            let key = join_key(parent, &name);
            let directory = entry.directory;
            tree.insert(key.clone(), entry);
            if tree.len() > MAX_ENTRIES {
                return Err(invalid("目录超过 100000 个项目"));
            }
            if directory {
                walk(root, &key, tree, cancel, exclude)?;
            }
        }
        Ok(())
    }
    let mut tree = Tree::new();
    walk(root, "", &mut tree, cancel, exclude)?;
    Ok(tree)
}
fn fat_time(entry: &Entry) -> fatfs::DateTime {
    let time = Local
        .timestamp_opt(entry.modified_seconds, entry.modified_nanos)
        .single()
        .unwrap_or_else(|| {
            Local
                .with_ymd_and_hms(1980, 1, 1, 0, 0, 0)
                .earliest()
                .unwrap()
        });
    let year = time.year().clamp(1980, 2107);
    let day = if chrono::NaiveDate::from_ymd_opt(year, time.month(), time.day()).is_some() {
        time.day()
    } else {
        28
    };
    fatfs::DateTime {
        date: fatfs::Date {
            year: year as u16,
            month: time.month() as u16,
            day: day as u16,
        },
        time: fatfs::Time {
            hour: time.hour() as u16,
            min: time.minute() as u16,
            sec: time.second() as u16,
            millis: 0,
        },
    }
}

impl Mapping {
    pub fn create(
        root: PathBuf,
        image: PathBuf,
        size_mib: u32,
        readonly: bool,
        cancel: &AtomicBool,
        progress: impl Fn(Progress),
    ) -> Result<Self> {
        if !(16..=2048).contains(&size_mib) {
            return Err(invalid("工作镜像大小必须为 16–2048 MiB"));
        }
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(invalid("请选择一个文件夹"));
        }
        let parent = image
            .parent()
            .ok_or_else(|| invalid("工作镜像路径无效"))?
            .canonicalize()?;
        let image = parent.join(
            image
                .file_name()
                .ok_or_else(|| invalid("工作镜像名称无效"))?,
        );
        if image.starts_with(&root) {
            return Err(invalid("工作镜像不能位于重定向的文件夹中"));
        }
        if image.exists() {
            return Err(invalid("工作镜像已经存在，请选择一个新文件名"));
        }
        if !readonly {
            NamedTempFile::new_in(&root)
                .map_err(|e| invalid(format!("文件夹不可写，请选择只读连接：{e}")))?;
        }
        cancelled(cancel)?;
        progress(Progress {
            stage: "扫描文件夹",
            files: 0,
            bytes: 0,
            total_bytes: 0,
        });
        let baseline = scan(&root, cancel)?;
        let image_baseline = baseline
            .iter()
            .map(|(p, e)| Ok((p.clone(), image_entry(e)?)))
            .collect::<Result<Tree>>()?;
        let total = baseline.values().map(|e| e.size).sum::<u64>();
        if total >= size_mib as u64 * 1_048_576 {
            return Err(invalid("工作镜像空间不足，请增大镜像容量"));
        }
        // FAT16 has a fixed root directory. fatfs writes VFAT slots even for
        // short names, so 512 entries cannot hold 256 ordinary root files.
        let root_slots = baseline
            .keys()
            .filter(|name| !name.contains('/'))
            .map(|name| name.encode_utf16().count().div_ceil(13) + 1)
            .sum::<usize>()
            + 1; // volume label
        let root_slots = if size_mib == 2048 {
            512
        } else {
            u16::try_from(root_slots.max(512).next_multiple_of(16)).map_err(|_| {
                invalid("根目录名称超过 FAT16 的容量，请选择 2048 MiB FAT32 镜像或使用子目录")
            })?
        };
        let mut temp = NamedTempFile::new_in(&parent)?;
        temp.as_file().set_len(size_mib as u64 * 1_048_576)?;
        fatfs::format_volume(
            temp.as_file_mut(),
            FormatVolumeOptions::new()
                .fat_type(if size_mib == 2048 {
                    FatType::Fat32
                } else {
                    FatType::Fat16
                })
                .max_root_dir_entries(root_slots)
                .bytes_per_sector(512)
                .volume_label(*b"AMIKVM     "),
        )?;
        {
            use std::io::{Seek, SeekFrom};
            temp.as_file_mut().seek(SeekFrom::Start(0))?;
            let filesystem = FileSystem::new(
                temp.as_file_mut(),
                FsOptions::new().update_accessed_date(false),
            )?;
            let mut bytes = 0;
            let mut files = 0;
            for (path, entry) in &baseline {
                cancelled(cancel)?;
                if entry.directory {
                    filesystem.root_dir().create_dir(path)?;
                } else {
                    let mut source = File::open(root.join(path))?;
                    let mut target = filesystem.root_dir().create_file(path)?;
                    let (copied, hash) = stream_copy(&mut source, &mut target, cancel)?;
                    if copied != entry.size || hash != entry.hash {
                        return Err(invalid(format!("生成镜像时文件发生变化：{path}")));
                    }
                    // fatfs deprecates this API in favour of a clock provider; setting
                    // a copied file's historical time still requires this setter.
                    #[allow(deprecated)]
                    target.set_modified(fat_time(entry));
                    target.flush()?;
                    bytes += copied;
                    files += 1;
                }
                progress(Progress {
                    stage: "生成 FAT 镜像",
                    files,
                    bytes,
                    total_bytes: total,
                });
            }
            // Child creation updates parent directories; set historical metadata last,
            // from deepest descendants to ancestors, after every open handle is gone.
            let mut paths = baseline.iter().collect::<Vec<_>>();
            paths.sort_by_key(|(path, _)| std::cmp::Reverse(path.split('/').count()));
            for (path, entry) in paths {
                cancelled(cancel)?;
                filesystem
                    .root_dir()
                    .set_metadata(path, fat_time(entry), entry.readonly)?;
            }
            filesystem.unmount()?;
        }
        cancelled(cancel)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&image)
            .map_err(|e| Error::Io(e.error))?;
        Ok(Self {
            root,
            image,
            size_mib,
            readonly,
            baseline,
            image_baseline: Some(image_baseline),
        })
    }

    pub fn prepare(&self, cancel: &AtomicBool, progress: impl Fn(Progress)) -> Result<Plan> {
        if self.readonly {
            return Err(invalid("只读映射无需同步"));
        }
        self.validate()?;
        self.recover()?;
        let image = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.image)?;
        image
            .try_lock_exclusive()
            .map_err(|e| invalid(format!("请先停止介质重定向，再同步文件夹：{e}")))?;
        if image.metadata()?.len() != self.size_mib as u64 * 1_048_576 {
            return Err(invalid("工作镜像大小已经改变"));
        }
        let staged = tempfile::Builder::new()
            .prefix(".amikvm-folder-read-")
            .tempdir_in(self.image.parent().unwrap())?;
        let length = image.metadata()?.len();
        let disk = BoundedDisk {
            file: &image,
            cancel,
            length,
            remaining_operations: 10_000_000,
            remaining_read: length * 4 + 16_777_216,
        };
        let filesystem = FileSystem::new(disk, FsOptions::new().update_accessed_date(false))?;
        let mut remote = Tree::new();
        let mut bytes = 0;
        fn walk<T: fatfs::ReadWriteSeek>(
            directory: fatfs::Dir<'_, T>,
            parent: &str,
            staging: &Path,
            tree: &mut Tree,
            bytes: &mut u64,
            limit: u64,
            cancel: &AtomicBool,
            progress: &impl Fn(Progress),
        ) -> Result<()> {
            if parent.split('/').count() > MAX_DEPTH {
                return Err(invalid("FAT 目录层级超过 128 层"));
            }
            let mut names = BTreeSet::new();
            for item in directory.iter() {
                cancelled(cancel)?;
                let item = item?;
                let name = item.file_name();
                if name == "." || name == ".." {
                    continue;
                }
                name_valid(&name)?;
                if !names.insert(name.to_uppercase()) {
                    return Err(invalid(format!("FAT 文件名冲突：{name}")));
                }
                let key = join_key(parent, &name);
                if tree.len() >= MAX_ENTRIES {
                    return Err(invalid("FAT 镜像超过 100000 个项目"));
                }
                let seconds = fat_seconds(item.modified())?;
                let mut entry = Entry {
                    directory: item.is_dir(),
                    size: 0,
                    hash: String::new(),
                    modified_seconds: seconds,
                    modified_nanos: 0,
                    readonly: item.attributes().contains(fatfs::FileAttributes::READ_ONLY),
                };
                if item.is_dir() {
                    fs::create_dir(staging.join(&key))?;
                    tree.insert(key.clone(), entry);
                    walk(
                        item.to_dir(),
                        &key,
                        staging,
                        tree,
                        bytes,
                        limit,
                        cancel,
                        progress,
                    )?;
                } else if item.is_file() {
                    if item.len() > limit.saturating_sub(*bytes) {
                        return Err(invalid("FAT 文件总大小超过镜像容量"));
                    }
                    let mut source = item.to_file().take(item.len() + 1);
                    let mut target = File::create(staging.join(&key))?;
                    (entry.size, entry.hash) = stream_copy(&mut source, &mut target, cancel)?;
                    if entry.size != item.len() {
                        return Err(invalid(format!("FAT 文件长度无效：{key}")));
                    }
                    *bytes += entry.size;
                    tree.insert(key, entry);
                } else {
                    return Err(invalid("FAT 目录包含无效项目"));
                }
                progress(Progress {
                    stage: "读取远程修改",
                    files: tree.len(),
                    bytes: *bytes,
                    total_bytes: limit,
                });
            }
            Ok(())
        }
        walk(
            filesystem.root_dir(),
            "",
            staged.path(),
            &mut remote,
            &mut bytes,
            self.size_mib as u64 * 1_048_576,
            cancel,
            &progress,
        )?;
        filesystem.unmount()?;
        progress(Progress {
            stage: "检查本地修改",
            files: 0,
            bytes: 0,
            total_bytes: 0,
        });
        let local = scan(&self.root, cancel)?;
        let keys: BTreeSet<_> = self.baseline.keys().chain(remote.keys()).cloned().collect();
        let mut changes = Vec::new();
        let mut conflicts = BTreeSet::new();
        let mut updates = Tree::new();
        let mut remote_files = BTreeSet::new();
        for path in keys {
            let before = self.baseline.get(&path);
            let after = remote.get(&path);
            let current = local.get(&path);
            let expected = self
                .image_baseline
                .as_ref()
                .and_then(|tree| tree.get(&path))
                .cloned()
                .or(before.map(image_entry).transpose()?);
            let content_changed = !same(expected.as_ref(), after);
            let metadata_changed = expected.as_ref().zip(after).is_some_and(|(a, b)| {
                (!a.directory || self.image_baseline.is_some())
                    && (!same_time(a, b) || a.readonly != b.readonly)
            });
            if !content_changed && !metadata_changed {
                continue;
            }
            match (before, expected.as_ref(), after, current) {
                (Some(before), Some(expected), Some(after), Some(current))
                    if before.directory == after.directory
                        && current.directory == after.directory =>
                {
                    let mut merged = current.clone();
                    if content_changed {
                        if !before.content_eq(current) && !after.content_eq(current) {
                            conflicts.insert(path.clone());
                        }
                        merged.size = after.size;
                        merged.hash = after.hash.clone();
                        if !after.directory {
                            remote_files.insert(path.clone());
                        }
                    }
                    if !same_time(expected, after)
                        && (!after.directory || self.image_baseline.is_some())
                    {
                        if !same_time(before, current)
                            && image_entry(current).map_or(true, |e| !same_time(&e, after))
                        {
                            conflicts.insert(path.clone());
                        }
                        merged.modified_seconds = after.modified_seconds;
                        merged.modified_nanos = after.modified_nanos;
                    }
                    if expected.readonly != after.readonly {
                        if current.readonly != before.readonly && current.readonly != after.readonly
                        {
                            conflicts.insert(path.clone());
                        }
                        merged.readonly = after.readonly;
                    }
                    updates.insert(path.clone(), merged);
                }
                _ => {
                    if !same_state(before, current) && !same_state(after, current) {
                        conflicts.insert(path.clone());
                    }
                    if let Some(after) = after {
                        updates.insert(path.clone(), after.clone());
                        if !after.directory {
                            remote_files.insert(path.clone());
                        }
                    }
                }
            }
            if after.is_none_or(|e| !e.directory) && before.is_some_and(|e| e.directory) {
                for (child, value) in &local {
                    if below(child, &path) && !same_state(Some(value), self.baseline.get(child)) {
                        conflicts.insert(child.clone());
                    }
                }
            }
            let mut parent = Path::new(&path).parent();
            while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
                let p = p.to_str().unwrap();
                if !local.get(p).is_some_and(|e| e.directory)
                    && remote.get(p).is_some_and(|e| e.directory)
                    && (self.baseline.contains_key(p) || local.contains_key(p))
                {
                    conflicts.insert(p.to_owned());
                }
                parent = Path::new(p).parent();
            }
            changes.push(Change {
                path,
                operation: if before.is_none() {
                    "新增"
                } else if after.is_none() {
                    "删除"
                } else {
                    if content_changed {
                        "修改"
                    } else {
                        "修改元数据"
                    }
                },
            });
        }
        Ok(Plan {
            changes,
            conflicts: conflicts.into_iter().collect(),
            local,
            remote,
            updates,
            remote_files,
            staged,
            _image_lock: image,
        })
    }

    pub fn apply(
        &self,
        plan: Plan,
        overwrite_conflicts: bool,
        cancel: &AtomicBool,
        progress: impl Fn(Progress),
    ) -> Result<()> {
        self.validate()?;
        if !plan.conflicts.is_empty() && !overwrite_conflicts {
            return Err(invalid(
                "存在本地修改冲突，请明确选择覆盖冲突，或保留工作镜像",
            ));
        }
        if scan(&self.root, cancel)? != plan.local {
            return Err(invalid("预览后本地文件夹发生变化，请重新预览"));
        }
        let mut desired = plan.local.clone();
        let mut changes = plan.changes.iter().collect::<Vec<_>>();
        changes.sort_by_key(|c| c.path.split('/').count());
        let mut roots = BTreeSet::new();
        for change in changes {
            if same_state(plan.local.get(&change.path), plan.updates.get(&change.path)) {
                continue;
            }
            roots.insert(change.path.split('/').next().unwrap().to_uppercase());
            match plan.updates.get(&change.path) {
                Some(entry) => {
                    if !entry.directory || desired.get(&change.path).is_some_and(|e| !e.directory) {
                        desired.retain(|p, _| !below(p, &change.path));
                    }
                    let mut parent = Path::new(&change.path).parent();
                    while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
                        let p = p.to_str().unwrap();
                        if !desired.get(p).is_some_and(|e| e.directory) {
                            desired.retain(|key, _| !below(key, p));
                            desired.insert(
                                p.into(),
                                plan.remote
                                    .get(p)
                                    .ok_or_else(|| invalid("远程目录结构无效"))?
                                    .clone(),
                            );
                        }
                        parent = Path::new(p).parent();
                    }
                    desired.insert(change.path.clone(), entry.clone());
                }
                None => desired.retain(|p, _| !below(p, &change.path)),
            }
        }
        cancelled(cancel)?;
        let originals: BTreeSet<String> = plan
            .local
            .keys()
            .filter_map(|p| {
                let name = p.split('/').next().unwrap();
                roots
                    .contains(&name.to_uppercase())
                    .then(|| name.to_owned())
            })
            .collect();
        let destinations: BTreeSet<String> = desired
            .keys()
            .filter_map(|p| {
                let name = p.split('/').next().unwrap();
                roots
                    .contains(&name.to_uppercase())
                    .then(|| name.to_owned())
            })
            .collect();
        let mut transaction = transaction::Transaction::begin(
            &self.root,
            &self.image,
            &originals,
            &destinations,
            cancel,
        )?;
        let prepared = transaction.prepared();
        let result = (|| -> Result<()> {
            for (index, root) in destinations.iter().enumerate() {
                cancelled(cancel)?;
                for (path, entry) in desired.iter().filter(|(p, _)| below(p, root)) {
                    cancelled(cancel)?;
                    let destination = prepared.join(path);
                    if entry.directory {
                        fs::create_dir_all(&destination)?;
                    } else {
                        fs::create_dir_all(destination.parent().unwrap())?;
                        let source = if plan.remote_files.contains(path) {
                            plan.staged.path().join(path)
                        } else {
                            self.root.join(path)
                        };
                        let mut temporary = NamedTempFile::new_in(destination.parent().unwrap())?;
                        let (bytes, hash) =
                            stream_copy(&mut File::open(source)?, &mut temporary, cancel)?;
                        if bytes != entry.size || hash != entry.hash {
                            return Err(invalid(format!("同步时文件发生变化：{path}")));
                        }
                        temporary.as_file().sync_all()?;
                        temporary
                            .persist(&destination)
                            .map_err(|e| Error::Io(e.error))?;
                        filetime::set_file_mtime(
                            &destination,
                            filetime::FileTime::from_unix_time(
                                entry.modified_seconds,
                                entry.modified_nanos,
                            ),
                        )?;
                        if let Ok(metadata) = fs::metadata(self.root.join(path)) {
                            if metadata.is_file() {
                                fs::set_permissions(&destination, metadata.permissions())?;
                            }
                        }
                        set_readonly(&destination, entry.readonly)?;
                    }
                    progress(Progress {
                        stage: "准备同步内容",
                        files: index + 1,
                        bytes: 0,
                        total_bytes: 0,
                    });
                }
                for (path, entry) in desired
                    .iter()
                    .rev()
                    .filter(|(p, e)| below(p, root) && e.directory)
                {
                    filetime::set_file_mtime(
                        prepared.join(path),
                        filetime::FileTime::from_unix_time(
                            entry.modified_seconds,
                            entry.modified_nanos,
                        ),
                    )?;
                    if let Ok(metadata) = fs::metadata(self.root.join(path)) {
                        fs::set_permissions(prepared.join(path), metadata.permissions())?;
                    }
                    set_readonly(&prepared.join(path), entry.readonly)?;
                }
            }
            if scan_excluding(&self.root, cancel, Some(transaction.backup()))? != plan.local {
                return Err(invalid("准备同步期间本地文件夹发生变化，请重新预览"));
            }
            transaction.ready(cancel)?;
            progress(Progress {
                stage: "准备完成",
                files: 0,
                bytes: 0,
                total_bytes: 0,
            });
            transaction.install(cancel, |files, backing_up| {
                progress(Progress {
                    stage: if backing_up {
                        "备份本地文件"
                    } else {
                        "同步到文件夹"
                    },
                    files,
                    bytes: 0,
                    total_bytes: 0,
                });
            })?;
            transaction.commit(cancel)
        })();
        if let Err(error) = result {
            if let Err(rollback) = transaction.rollback() {
                return Err(invalid(format!("{error}；恢复未完成：{rollback}")));
            }
            return Err(error);
        }
        // The image is retained until the native manager records the completed outcome.
        Ok(())
    }

    /// Restore an interrupted writeback before another preview or discard.
    pub fn recover(&self) -> Result<()> {
        self.validate()?;
        transaction::recover(&self.root, &self.image)
    }

    fn validate(&self) -> Result<()> {
        if !fs::symlink_metadata(&self.root)?.is_dir() || self.root.canonicalize()? != self.root {
            return Err(invalid("原始文件夹已移动或被替换"));
        }
        if !(16..=2048).contains(&self.size_mib) || self.image.starts_with(&self.root) {
            return Err(invalid("文件夹映射路径或容量无效"));
        }
        Ok(())
    }
    pub fn discard(&self) -> Result<()> {
        self.recover()?;
        match OpenOptions::new().read(true).write(true).open(&self.image) {
            Ok(file) => {
                file.try_lock_exclusive()
                    .map_err(|e| invalid(format!("工作镜像仍在使用：{e}")))?;
                drop(file);
                fs::remove_file(&self.image)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
}
fn remove_entry(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            // This helper is used only for transaction-owned output and backup trees.
            // Make read-only copies removable without following symbolic links.
            let mut permissions = metadata.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(permissions.mode() | 0o700);
            }
            #[cfg(not(unix))]
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions)?;
            for entry in fs::read_dir(path)? {
                remove_entry(&entry?.path())?;
            }
            fs::remove_dir(path)?;
        }
        Ok(metadata) => {
            #[cfg(windows)]
            {
                if metadata.file_type().is_symlink() && metadata.is_dir() {
                    fs::remove_dir(path)?;
                    return Ok(());
                }
                if !metadata.file_type().is_symlink() && metadata.permissions().readonly() {
                    let mut permissions = metadata.permissions();
                    permissions.set_readonly(false);
                    fs::set_permissions(path, permissions)?;
                }
            }
            #[cfg(not(windows))]
            let _ = metadata;
            fs::remove_file(path)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
fn set_readonly(path: &Path, readonly: bool) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    if permissions.readonly() != readonly {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = permissions.mode();
            permissions.set_mode(if readonly {
                mode & !0o222
            } else {
                mode | 0o200
            });
        }
        #[cfg(not(unix))]
        permissions.set_readonly(readonly);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}
