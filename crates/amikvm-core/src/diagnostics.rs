//! Bounded diagnostics, optional console/file output and atomic exports.
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread::JoinHandle,
};
use uuid::Uuid;

pub const CAPACITY: usize = 1000;
pub const PAGE_SIZE: usize = 100;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Debug,
    #[default]
    Info,
    Warning,
    Error,
}
impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Self::Debug => "调试",
            Self::Info => "信息",
            Self::Warning => "警告",
            Self::Error => "错误",
        }
    }
    pub fn value(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Application,
    Server,
    Session,
    Protocol,
    Input,
    Media,
    Folder,
    Recording,
    Playback,
    Capture,
    Sharing,
    Ipmi,
    Interface,
}
impl Category {
    pub const ALL: [Self; 13] = [
        Self::Application,
        Self::Server,
        Self::Session,
        Self::Protocol,
        Self::Input,
        Self::Media,
        Self::Folder,
        Self::Recording,
        Self::Playback,
        Self::Capture,
        Self::Sharing,
        Self::Ipmi,
        Self::Interface,
    ];
    pub fn value(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Server => "server",
            Self::Session => "session",
            Self::Protocol => "protocol",
            Self::Input => "input",
            Self::Media => "media",
            Self::Folder => "folder",
            Self::Recording => "recording",
            Self::Playback => "playback",
            Self::Capture => "capture",
            Self::Sharing => "sharing",
            Self::Ipmi => "ipmi",
            Self::Interface => "interface",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Application => "应用程序",
            Self::Server => "服务器",
            Self::Session => "会话",
            Self::Protocol => "协议",
            Self::Input => "键盘与鼠标",
            Self::Media => "虚拟介质",
            Self::Folder => "文件夹",
            Self::Recording => "录制",
            Self::Playback => "回放",
            Self::Capture => "画面捕获",
            Self::Sharing => "共享权限",
            Self::Ipmi => "IPMI",
            Self::Interface => "界面",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub sequence: u64,
    pub timestamp: String,
    pub level: Level,
    pub category: Category,
    pub server_id: Option<Uuid>,
    pub source: &'static str,
    pub details: String,
}

#[derive(Debug, Clone)]
pub struct Filter {
    pub minimum: Level,
    pub category: Option<Category>,
    pub server: Option<Uuid>,
    pub query: String,
    pub source_terms: Vec<String>,
    pub page: usize,
}
impl Default for Filter {
    fn default() -> Self {
        Self {
            minimum: Level::Debug,
            category: None,
            server: None,
            query: String::new(),
            source_terms: vec![],
            page: 0,
        }
    }
}

#[derive(Default, Clone)]
pub struct Snapshot {
    pub enabled: bool,
    pub minimum: Level,
    pub console: bool,
    pub file: Option<PathBuf>,
    pub file_active: bool,
    pub file_error: Option<String>,
    pub file_dropped: u64,
    pub retained: usize,
    pub total: u64,
    pub matched: usize,
    pub page: usize,
    pub pages: usize,
    pub entries: Vec<Entry>,
}

struct FileSink {
    path: PathBuf,
    sender: Option<mpsc::SyncSender<Vec<u8>>>,
    worker: Option<JoinHandle<Result<(), String>>>,
    failure: Arc<Mutex<Option<String>>>,
}
impl FileSink {
    fn open(path: PathBuf) -> Result<Self, String> {
        let mut options = OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| e.to_string())?;
        fs2::FileExt::try_lock_exclusive(&file)
            .map_err(|_| "日志文件正在使用，请选择其他文件。".to_owned())?;
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(1024);
        let failure = Arc::new(Mutex::new(None));
        let result = failure.clone();
        let worker = std::thread::Builder::new()
            .name("amikvm-log-writer".into())
            .spawn(move || {
                let output = (|| -> std::io::Result<()> {
                    // Keep an existing file's final record separate when appending.
                    if file.metadata()?.len() != 0 {
                        file.write_all(b"\n")?;
                    }
                    for bytes in receiver {
                        file.write_all(&bytes)?;
                        file.flush()?;
                    }
                    file.sync_all()
                })()
                .map_err(|error| error.to_string());
                if let Err(error) = &output {
                    *result.lock().unwrap() = Some(error.clone());
                }
                output
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            path,
            sender: Some(sender),
            worker: Some(worker),
            failure,
        })
    }
    fn stop(&mut self) -> Result<(), String> {
        self.sender.take();
        self.worker.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| "Log writer panicked".to_owned())?
        })
    }
}
impl Drop for FileSink {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct State {
    enabled: bool,
    minimum: Level,
    console: bool,
    next: u64,
    entries: VecDeque<Entry>,
    sink: Option<FileSink>,
    file_error: Option<String>,
    file_dropped: u64,
    secrets: Vec<String>,
    observations: HashMap<(Category, Option<Uuid>, String), String>,
}

pub struct Recorder {
    state: Mutex<State>,
    file_operation: Mutex<()>,
}
impl Default for Recorder {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                enabled: true,
                minimum: Level::Info,
                console: false,
                next: 1,
                entries: VecDeque::new(),
                sink: None,
                file_error: None,
                file_dropped: 0,
                secrets: vec![],
                observations: HashMap::new(),
            }),
            file_operation: Mutex::new(()),
        }
    }
}
impl Recorder {
    pub fn protect(&self, secret: &str) {
        if secret.is_empty() {
            return;
        }
        let quoted = serde_json::to_string(secret).expect("serializable secret");
        let escaped = quoted[1..quoted.len() - 1].to_owned();
        let encoded = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect::<String>();
        let mut state = self.state.lock().unwrap();
        for variant in [secret.to_owned(), escaped, encoded] {
            if !state.secrets.contains(&variant) {
                state.secrets.push(variant);
            }
        }
        state.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }
    pub fn sanitize(&self, details: &str) -> String {
        redact(details, &self.state.lock().unwrap().secrets)
    }
    pub fn configure(&self, enabled: bool, minimum: Level, console: bool) {
        let mut state = self.state.lock().unwrap();
        state.enabled = enabled;
        state.minimum = minimum;
        state.console = console;
    }
    pub fn push(
        &self,
        level: Level,
        category: Category,
        server: Option<Uuid>,
        source: &'static str,
        details: impl AsRef<str>,
    ) {
        let mut state = self.state.lock().unwrap();
        Self::append(
            &mut state,
            level,
            category,
            server,
            source,
            details.as_ref(),
        );
    }
    fn append(
        state: &mut State,
        level: Level,
        category: Category,
        server_id: Option<Uuid>,
        source: &'static str,
        details: &str,
    ) {
        if !state.enabled || level < state.minimum {
            return;
        }
        let entry = Entry {
            sequence: state.next,
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            level,
            category,
            server_id,
            source,
            details: redact(details, &state.secrets),
        };
        state.next = state.next.saturating_add(1);
        let mut bytes = serde_json::to_vec(&entry).expect("serializable log entry");
        bytes.push(b'\n');
        if state.console {
            let _ = std::io::stderr().write_all(&bytes);
        }
        if let Some(sink) = &state.sink {
            if let Some(sender) = &sink.sender {
                if let Err(error) = sender.try_send(bytes) {
                    match error {
                        mpsc::TrySendError::Full(_) => {
                            state.file_dropped = state.file_dropped.saturating_add(1)
                        }
                        mpsc::TrySendError::Disconnected(_) => {
                            state.file_error = sink
                                .failure
                                .lock()
                                .unwrap()
                                .clone()
                                .or_else(|| Some("Log writer stopped".into()))
                        }
                    }
                }
            }
        }
        if state.entries.len() == CAPACITY {
            state.entries.pop_front();
        }
        state.entries.push_back(entry);
    }
    pub fn changed(
        &self,
        category: Category,
        server: Option<Uuid>,
        key: &str,
        fingerprint: impl AsRef<str>,
        level: Level,
        source: &'static str,
        details: impl AsRef<str>,
    ) {
        let mut state = self.state.lock().unwrap();
        let slot = (category, server, key.to_owned());
        let fingerprint = fingerprint.as_ref();
        if state
            .observations
            .get(&slot)
            .is_some_and(|old| old == fingerprint)
        {
            return;
        }
        state.observations.insert(slot, fingerprint.to_owned());
        Self::append(
            &mut state,
            level,
            category,
            server,
            source,
            details.as_ref(),
        );
    }
    fn matches(entry: &Entry, filter: &Filter) -> bool {
        entry.level >= filter.minimum
            && filter.category.is_none_or(|c| c == entry.category)
            && filter.server.is_none_or(|s| entry.server_id == Some(s))
            && (filter.query.is_empty()
                || filter.source_terms.iter().any(|term| {
                    entry.source == term
                        || entry.category.label() == term
                        || entry.level.label() == term
                })
                || format!(
                    "{} {} {} {}",
                    entry.source,
                    entry.details,
                    entry.timestamp,
                    entry.server_id.map(|s| s.to_string()).unwrap_or_default()
                )
                .to_lowercase()
                .contains(&filter.query.to_lowercase()))
    }
    pub fn snapshot(&self, filter: &Filter) -> Snapshot {
        let state = self.state.lock().unwrap();
        let matched = state
            .entries
            .iter()
            .filter(|e| Self::matches(e, filter))
            .count();
        let pages = matched.div_ceil(PAGE_SIZE).max(1);
        let page = filter.page.min(pages - 1);
        Snapshot {
            enabled: state.enabled,
            minimum: state.minimum,
            console: state.console,
            file: state.sink.as_ref().map(|s| s.path.clone()),
            file_active: state
                .sink
                .as_ref()
                .is_some_and(|s| s.failure.lock().unwrap().is_none()),
            file_error: state
                .sink
                .as_ref()
                .and_then(|s| s.failure.lock().unwrap().clone())
                .or_else(|| state.file_error.clone()),
            file_dropped: state.file_dropped,
            retained: state.entries.len(),
            total: state.next - 1,
            matched,
            page,
            pages,
            entries: state
                .entries
                .iter()
                .rev()
                .filter(|e| Self::matches(e, filter))
                .skip(page * PAGE_SIZE)
                .take(PAGE_SIZE)
                .cloned()
                .collect(),
        }
    }
    pub fn clear(&self) {
        self.state.lock().unwrap().entries.clear();
    }
    pub fn export(&self, filter: &Filter, path: &Path) -> Result<usize, String> {
        let _operation = self.file_operation.lock().unwrap();
        // Locks cover aliases and hard links to a live append file as well.
        let existing = match std::fs::File::open(path) {
            Ok(file) => {
                fs2::FileExt::try_lock_exclusive(&file)
                    .map_err(|_| "日志文件正在使用，请选择其他文件。".to_owned())?;
                Some(file)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        if self
            .state
            .lock()
            .unwrap()
            .sink
            .as_ref()
            .is_some_and(|sink| sink.path == path)
        {
            return Err("请停止文件日志后再覆盖该文件。".into());
        }
        let entries = {
            let state = self.state.lock().unwrap();
            state
                .entries
                .iter()
                .filter(|e| Self::matches(e, filter))
                .cloned()
                .collect::<Vec<_>>()
        };
        let parent = path.parent().ok_or("Invalid log export path")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        for entry in &entries {
            serde_json::to_writer(&mut temporary, entry).map_err(|e| e.to_string())?;
            temporary.write_all(b"\n").map_err(|e| e.to_string())?;
        }
        temporary.as_file().sync_all().map_err(|e| e.to_string())?;
        drop(existing);
        temporary.persist(path).map_err(|e| e.error.to_string())?;
        Ok(entries.len())
    }
    pub fn start_file(&self, path: PathBuf) -> Result<(), String> {
        let _operation = self.file_operation.lock().unwrap();
        let sink = FileSink::open(path)?;
        self.stop_locked()?;
        let mut state = self.state.lock().unwrap();
        state.sink = Some(sink);
        state.file_error = None;
        state.file_dropped = 0;
        state.enabled = true;
        Self::append(
            &mut state,
            Level::Info,
            Category::Application,
            None,
            "文件日志已开始",
            env!("CARGO_PKG_VERSION"),
        );
        Ok(())
    }
    fn stop_locked(&self) -> Result<(), String> {
        let sink = self.state.lock().unwrap().sink.take();
        if let Some(mut sink) = sink {
            let result = sink.stop();
            if let Err(error) = &result {
                self.state.lock().unwrap().file_error = Some(error.clone());
            }
            result
        } else {
            Ok(())
        }
    }
    pub fn stop_file(&self) -> Result<(), String> {
        let _operation = self.file_operation.lock().unwrap();
        self.stop_locked()
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.stop_file();
    }
}

fn redact(details: &str, secrets: &[String]) -> String {
    static URL: OnceLock<regex::Regex> = OnceLock::new();
    static CREDENTIAL: OnceLock<regex::Regex> = OnceLock::new();
    static JSON_CREDENTIAL: OnceLock<regex::Regex> = OnceLock::new();
    let urls = URL.get_or_init(|| regex::Regex::new(r#"https?://[^\s\"<>]+"#).unwrap());
    let credentials=CREDENTIAL.get_or_init(||regex::Regex::new(r#"(?i)\b(password|passwd|pwd|token|csrf|cookie|authorization|session[_-]?id|sessiontoken|qsessionid|x-csrf-token)\s*[:=]\s*(\"[^\"]*\"|'[^']*'|[^\s,;]+)"#).unwrap());
    let mut clean = urls
        .replace_all(details, |capture: &regex::Captures| {
            url::Url::parse(&capture[0])
                .map(|mut url| {
                    let _ = url.set_username("");
                    let _ = url.set_password(None);
                    url.set_query(None);
                    url.set_fragment(None);
                    url.to_string()
                })
                .unwrap_or_else(|_| "[redacted URL]".into())
        })
        .into_owned();
    let json_credentials=JSON_CREDENTIAL.get_or_init(||regex::Regex::new(r#"(?i)("(?:password|passwd|pwd|token|csrf|cookie|authorization|session[_-]?id|sessiontoken|qsessionid|x-csrf-token)"\s*:\s*)"(?:\\.|[^"\\])*""#).unwrap());
    clean = json_credentials
        .replace_all(&clean, "${1}\"[redacted]\"")
        .into_owned();
    clean = credentials
        .replace_all(&clean, "$1=[redacted]")
        .into_owned();
    for secret in secrets {
        clean = clean.replace(secret, "[redacted]");
    }
    clean.chars().take(4096).collect()
}
