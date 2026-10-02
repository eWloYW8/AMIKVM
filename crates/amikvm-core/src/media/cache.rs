//! CD image read-ahead, independent of the BMC's negotiated Boost mode.
//! JViewer com.ami.kvm.a.* uses 256 blocks of 64 logical 2048-byte sectors.
use super::nrg::Track;
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    io,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::SystemTime,
};

const SECTOR: usize = 2048;
const PAGE: u64 = 64;
const PAGES: usize = 256;
type Sense = [u8; 3];

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub enabled: bool,
    pub bytes: u64,
    pub pages: usize,
    pub hits: u64,
    pub misses: u64,
    pub foreground_reads: u64,
    pub prefetched_bytes: u64,
    pub prefetch_errors: u64,
    pub invalidations: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    bytes: u64,
    modified: Option<SystemTime>,
}
struct Source {
    file: File,
    tracks: Vec<Track>,
    blocks: u64,
}
impl Source {
    fn stamp(&self) -> io::Result<Stamp> {
        let metadata = self.file.metadata()?;
        Ok(Stamp {
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
    fn read(&self, lba: u64, count: u32) -> Result<Vec<u8>, Sense> {
        if lba
            .checked_add(u64::from(count))
            .is_none_or(|end| end > self.blocks)
        {
            return Err([5, 0x21, 0]);
        }
        let mut data = vec![0; count as usize * SECTOR];
        let mut completed = 0;
        while completed < u64::from(count) {
            let position = lba + completed;
            let track = self
                .tracks
                .iter()
                .find(|t| position >= t.lba && position < t.lba + t.blocks)
                .ok_or([5, 0x21, 0])?;
            let data_offset = usize::from(track.data_offset.ok_or([5, 0x64, 0])?);
            let sectors = (u64::from(count) - completed).min(track.lba + track.blocks - position);
            let offset = (position - track.lba)
                .checked_mul(u64::from(track.sector_size))
                .and_then(|n| track.offset.checked_add(n))
                .ok_or([5, 0x21, 0])?;
            if track.sector_size == SECTOR as u16 {
                exact_at(
                    &self.file,
                    &mut data[completed as usize * SECTOR..(completed + sectors) as usize * SECTOR],
                    offset,
                )
                .map_err(|_| [3, 0x11, 0])?;
            } else {
                let mut sector = vec![0; usize::from(track.sector_size)];
                for i in 0..sectors {
                    exact_at(
                        &self.file,
                        &mut sector,
                        offset + i * u64::from(track.sector_size),
                    )
                    .map_err(|_| [3, 0x11, 0])?;
                    if matches!(data_offset, 8 | 24) && sector[data_offset - 6] & 32 != 0 {
                        return Err([5, 0x64, 0]);
                    }
                    let bytes = sector
                        .get(data_offset..data_offset + SECTOR)
                        .ok_or([5, 0x64, 0])?;
                    let start = (completed + i) as usize * SECTOR;
                    data[start..start + SECTOR].copy_from_slice(bytes);
                }
            }
            completed += sectors;
        }
        Ok(data)
    }
}
fn exact_at(file: &File, mut data: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !data.is_empty() {
        #[cfg(unix)]
        let result = std::os::unix::fs::FileExt::read_at(file, data, offset);
        #[cfg(windows)]
        let result = std::os::windows::fs::FileExt::seek_read(file, data, offset);
        let count = match result {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        offset += count as u64;
        data = &mut data[count..];
    }
    Ok(())
}
#[derive(Clone, Copy)]
struct Plan {
    start: u64,
    version: u64,
    stamp: Stamp,
}
struct State {
    entries: HashMap<u64, Arc<Vec<u8>>>,
    recent: VecDeque<u64>,
    plan: Option<Plan>,
    version: u64,
    stamp: Stamp,
    closed: bool,
    stats: Stats,
}
impl State {
    fn touch(&mut self, page: u64) {
        if let Some(i) = self.recent.iter().position(|p| *p == page) {
            self.recent.remove(i);
        }
        self.recent.push_front(page);
    }
    fn insert(&mut self, page: u64, data: Vec<u8>) {
        if self.entries.contains_key(&page) {
            return;
        }
        if self.entries.len() == PAGES {
            if let Some(old) = self.recent.pop_back() {
                if let Some(data) = self.entries.remove(&old) {
                    self.stats.bytes -= data.len() as u64;
                }
            }
        }
        self.stats.bytes += data.len() as u64;
        self.entries.insert(page, Arc::new(data));
        self.touch(page);
        self.stats.pages = self.entries.len();
    }
    fn clear(&mut self, stamp: Stamp) {
        self.version = self.version.wrapping_add(1);
        self.plan = None;
        self.entries.clear();
        self.recent.clear();
        self.stamp = stamp;
        self.stats.bytes = 0;
        self.stats.pages = 0;
        self.stats.invalidations += 1;
    }
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}
pub(super) struct ReadAhead {
    source: Source,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl ReadAhead {
    pub fn new(file: &File, tracks: &[Track], blocks: u64) -> io::Result<Self> {
        let source = Source {
            file: file.try_clone()?,
            tracks: tracks.to_vec(),
            blocks,
        };
        let background = Source {
            file: file.try_clone()?,
            tracks: tracks.to_vec(),
            blocks,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                entries: HashMap::new(),
                recent: VecDeque::new(),
                plan: None,
                version: 0,
                stamp: source.stamp()?,
                closed: false,
                stats: Stats {
                    enabled: true,
                    ..Stats::default()
                },
            }),
            wake: Condvar::new(),
        });
        let worker_state = shared.clone();
        let worker = thread::Builder::new()
            .name("amikvm-media-prefetch".into())
            .spawn(move || prefetch(background, worker_state))?;
        Ok(Self {
            source,
            shared,
            worker: Some(worker),
        })
    }
    pub fn stats(&self) -> Stats {
        self.shared
            .state
            .lock()
            .map(|s| s.stats)
            .unwrap_or_default()
    }
    pub fn clear(&self) {
        if let Ok(mut state) = self.shared.state.lock() {
            let stamp = state.stamp;
            state.clear(stamp);
        }
        self.shared.wake.notify_all();
    }
    pub fn stop(mut self) -> Stats {
        self.close();
        self.stats()
    }
    fn close(&mut self) {
        if self.worker.is_none() {
            return;
        }
        if let Ok(mut state) = self.shared.state.lock() {
            state.closed = true;
            let stamp = state.stamp;
            state.clear(stamp);
            state.stats.enabled = false;
        }
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    fn validate(&self, stamp: Stamp) -> Result<(), Sense> {
        let mut state = self.shared.state.lock().map_err(|_| [3, 0x11, 0])?;
        if state.stamp != stamp {
            state.clear(stamp);
            self.shared.wake.notify_all();
            return Err([6, 0x28, 0]);
        }
        Ok(())
    }
    pub fn read(&mut self, lba: u64, count: u32) -> Result<Vec<u8>, Sense> {
        if count == 0 {
            return Ok(vec![]);
        }
        let end = lba
            .checked_add(u64::from(count))
            .filter(|end| *end <= self.source.blocks)
            .ok_or([5, 0x21, 0])?;
        let stamp = self.source.stamp().map_err(|_| [3, 0x11, 0])?;
        self.validate(stamp)?;
        let first = lba / PAGE * PAGE;
        let last = (end - 1) / PAGE * PAGE;
        let mut slices = vec![];
        {
            let mut state = self.shared.state.lock().map_err(|_| [3, 0x11, 0])?;
            for page in (first..=last).step_by(PAGE as usize) {
                let Some(data) = state.entries.get(&page).cloned() else {
                    slices.clear();
                    break;
                };
                let from = lba.saturating_sub(page) as usize * SECTOR;
                let to = (end - page).min(PAGE) as usize * SECTOR;
                if to > data.len() {
                    slices.clear();
                    break;
                }
                state.touch(page);
                slices.push((data, from, to));
            }
            if slices.is_empty() {
                state.stats.misses += 1;
                state.stats.foreground_reads += 1;
            } else {
                state.stats.hits += 1;
            }
        }
        if !slices.is_empty() {
            let mut data = Vec::with_capacity(count as usize * SECTOR);
            for (page, from, to) in slices {
                data.extend_from_slice(&page[from..to]);
            }
            self.validate(self.source.stamp().map_err(|_| [3, 0x11, 0])?)?;
            return Ok(data);
        }
        // Demand reads only touch the requested range. An unreadable future NRG
        // track or a short final page cannot fail an otherwise valid request.
        let data = self.source.read(lba, count)?;
        self.validate(self.source.stamp().map_err(|_| [3, 0x11, 0])?)?;
        let mut state = self.shared.state.lock().map_err(|_| [3, 0x11, 0])?;
        for page in (first..=last).step_by(PAGE as usize) {
            let page_end = (page + PAGE).min(self.source.blocks);
            if page >= lba && page_end <= end {
                state.insert(
                    page,
                    data[(page - lba) as usize * SECTOR..(page_end - lba) as usize * SECTOR]
                        .to_vec(),
                );
            }
        }
        state.version = state.version.wrapping_add(1);
        state.plan = Some(Plan {
            start: first,
            version: state.version,
            stamp,
        });
        self.shared.wake.notify_one();
        Ok(data)
    }
}
impl Drop for ReadAhead {
    fn drop(&mut self) {
        self.close();
    }
}
fn prefetch(source: Source, shared: Arc<Shared>) {
    loop {
        let plan = {
            let Ok(mut state) = shared.state.lock() else {
                return;
            };
            while state.plan.is_none() && !state.closed {
                let Ok(next) = shared.wake.wait(state) else {
                    return;
                };
                state = next;
            }
            if state.closed {
                return;
            }
            state.plan.take().expect("available plan")
        };
        for i in 0..PAGES {
            let page = plan.start + i as u64 * PAGE;
            if page >= source.blocks {
                break;
            }
            {
                let Ok(state) = shared.state.lock() else {
                    return;
                };
                if state.closed {
                    return;
                }
                if state.version != plan.version {
                    break;
                }
                if state.entries.contains_key(&page) {
                    continue;
                }
            }
            let result = source.read(page, PAGE.min(source.blocks - page) as u32);
            let stamp = source.stamp();
            let Ok(mut state) = shared.state.lock() else {
                return;
            };
            if state.closed {
                return;
            }
            if state.version != plan.version {
                break;
            }
            if stamp.ok() != Some(plan.stamp) {
                // Foreground validation reports the media change and establishes
                // the new stamp. Never publish a page read during that change.
                state.stats.prefetch_errors += 1;
                break;
            }
            match result {
                Ok(data) => {
                    state.stats.prefetched_bytes += data.len() as u64;
                    state.insert(page, data);
                }
                Err(_) => {
                    state.stats.prefetch_errors += 1;
                    break;
                }
            }
        }
    }
}
