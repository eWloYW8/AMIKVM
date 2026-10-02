//! BMC recording catalog, access checks and download locks (JViewer f/g, gui/af).
use crate::{
    Error, Result,
    auth::{Fields, WebSession},
    server::ApiMode,
};
use reqwest::{Method, header};
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashSet, path::Path, time::Duration};
use tokio::{io::AsyncWriteExt, sync::watch};

const MAX_CATALOG: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

/// Distinguish an explicit refusal from a lost/malformed lock response. The
/// latter may have acquired a lock on the BMC and needs a release attempt.
struct AccessFailure {
    error: Error,
    rejected: bool,
}
impl From<Error> for AccessFailure {
    fn from(error: Error) -> Self {
        Self {
            error,
            rejected: false,
        }
    }
}
impl From<reqwest::Error> for AccessFailure {
    fn from(error: reqwest::Error) -> Self {
        Error::from(error).into()
    }
}
impl From<AccessFailure> for Error {
    fn from(failure: AccessFailure) -> Self {
        failure.error
    }
}
impl std::fmt::Display for AccessFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub file: String,
    pub name: String,
    pub info: String,
}

fn valid_file(file: &str) -> Result<()> {
    if file.is_empty()
        || file.len() > 4096
        || file.chars().any(char::is_control)
        || file.contains('\\')
        || file.split('/').any(|part| part == ".." || part == ".")
    {
        return Err(Error::Protocol(
            "BMC returned an invalid recording name".into(),
        ));
    }
    Ok(())
}

/// Parse data only. RPC replies are JavaScript object literals, never evaluated.
pub fn parse_catalog(source: &str, mode: ApiMode) -> Result<Vec<Entry>> {
    if source.len() > MAX_CATALOG {
        return Err(Error::Protocol("BMC recording catalog is too large".into()));
    }
    check_status(source)?;
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |file: String, info: String| -> Result<()> {
        valid_file(&file)?;
        if seen.insert(file.clone()) {
            if entries.len() >= MAX_ENTRIES {
                return Err(Error::Protocol("Too many BMC recordings".into()));
            }
            entries.push(Entry {
                name: file.rsplit('/').next().unwrap_or(&file).into(),
                file,
                info,
            });
        }
        Ok(())
    };
    match mode {
        ApiMode::Rest => {
            fn visit(
                value: &Value,
                add: &mut impl FnMut(String, String) -> Result<()>,
            ) -> Result<()> {
                match value {
                    Value::Object(object) => {
                        if let Some(file) = object.get("file") {
                            let file = file.as_str().ok_or_else(|| {
                                Error::Protocol("Invalid recording filename".into())
                            })?;
                            let info = object
                                .get("fileinfo")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            add(file.into(), info.into())?;
                        } else {
                            for value in object.values() {
                                visit(value, add)?;
                            }
                        }
                    }
                    Value::Array(values) => {
                        for value in values {
                            visit(value, add)?;
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
            let value: Value = serde_json::from_str(source)?;
            visit(&value, &mut add)?;
        }
        ApiMode::Rpc => {
            let mut starts = Vec::new();
            let mut at = 0;
            while at < source.len() {
                match source.as_bytes()[at] {
                    b'\'' | b'"' => {
                        let (_, end) = js_string(source, at)?;
                        at = end;
                        continue;
                    }
                    b'{' => {
                        if let Some((_, child)) = starts.last_mut() {
                            *child = true;
                        }
                        starts.push((at, false));
                    }
                    b'}' => {
                        let (start, child) = starts.pop().ok_or_else(|| {
                            Error::Protocol("Invalid RPC recording catalog".into())
                        })?;
                        let object = &source[start..=at];
                        // Only leaf objects own records; don't duplicate records from envelopes.
                        if !child {
                            if let Some(file) = js_field(object, "FILE_NAME")? {
                                add(file, js_field(object, "FILE_INFO")?.unwrap_or_default())?;
                            }
                        }
                    }
                    _ => {}
                }
                at += 1;
            }
            if !starts.is_empty() {
                return Err(Error::Protocol("Truncated RPC recording catalog".into()));
            }
        }
        ApiMode::Auto => return Err(Error::Invalid("Recording API was not discovered".into())),
    }
    Ok(entries)
}

fn js_field(object: &str, key: &str) -> Result<Option<String>> {
    let pattern = format!(r#"(?:^|[{{,\s])['\"]?{}['\"]?\s*:\s*"#, regex::escape(key));
    let regex = regex::Regex::new(&pattern).map_err(|e| Error::Invalid(e.to_string()))?;
    regex
        .find(object)
        .map(|found| js_string(object, found.end()).map(|(s, _)| s))
        .transpose()
}

fn js_string(source: &str, start: usize) -> Result<(String, usize)> {
    let bytes = source.as_bytes();
    let quote = bytes
        .get(start)
        .copied()
        .filter(|b| matches!(b, b'\'' | b'"'))
        .ok_or_else(|| Error::Protocol("Expected an RPC quoted string".into()))?;
    let mut json = String::from("\"");
    let mut at = start + 1;
    while at < bytes.len() {
        let ch = source[at..].chars().next().unwrap();
        at += ch.len_utf8();
        if ch as u32 == quote as u32 {
            json.push('"');
            return Ok((serde_json::from_str(&json)?, at));
        }
        match ch {
            '\\' => {
                let escaped = *bytes
                    .get(at)
                    .ok_or_else(|| Error::Protocol("Truncated RPC string escape".into()))?;
                at += 1;
                match escaped {
                    b'\'' => json.push('\''),
                    b'x' => {
                        let hex = source
                            .get(at..at + 2)
                            .ok_or_else(|| Error::Protocol("Invalid RPC hex escape".into()))?;
                        let value = u8::from_str_radix(hex, 16)
                            .map_err(|_| Error::Protocol("Invalid RPC hex escape".into()))?;
                        json.push_str(&format!("\\u{value:04x}"));
                        at += 2;
                    }
                    _ => {
                        json.push('\\');
                        json.push(escaped as char);
                    }
                }
            }
            '"' => json.push_str("\\\""),
            _ => json.push(ch),
        }
    }
    Err(Error::Protocol("Truncated RPC quoted string".into()))
}

fn check_status(source: &str) -> Result<()> {
    let fields = Fields::parse(source.to_owned());
    if let Some(status) = fields
        .text("HAPI_STATUS")
        .and_then(|v| v.parse::<i64>().ok())
    {
        if status < 0 {
            return Err(match status {
                -5 => Error::Invalid("BMC 录像正在写入或被其他用户占用".into()),
                -6 => Error::Authentication("BMC recording access expired".into()),
                _ => Error::Protocol(format!("BMC recording request rejected ({status})")),
            });
        }
    }
    if fields.number("code") == Some(522) {
        return Err(Error::Invalid("BMC 录像正在写入或被其他用户占用".into()));
    }
    if let Some(code) = fields.number("code").filter(|code| *code >= 400) {
        return Err(Error::Protocol(format!(
            "BMC recording request rejected ({code})"
        )));
    }
    Ok(())
}

impl WebSession {
    fn recording_request(
        &self,
        client: &reqwest::Client,
        method: Method,
        url: url::Url,
    ) -> reqwest::RequestBuilder {
        let cookie = self.http_cookie();
        let mut request = client.request(method, url).header(header::COOKIE, cookie);
        if let Some(csrf) = &self.csrf {
            request = request.header("X-CSRFTOKEN", csrf);
        }
        request
    }

    async fn recording_text(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
    ) -> std::result::Result<String, AccessFailure> {
        let mut url = url::Url::parse(&format!("{}{path}", self.server.origin()))
            .map_err(|e| Error::Invalid(e.to_string()))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let mut response = self
            .recording_request(&self.client, method, url)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(AccessFailure {
                rejected: true,
                error: Error::Protocol(format!(
                    "BMC recording request: HTTP {}",
                    response.status().as_u16()
                )),
            });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > MAX_CATALOG.saturating_sub(body.len()) {
                return Err(Error::Protocol("BMC recording response is too large".into()).into());
            }
            body.extend_from_slice(&chunk);
        }
        let source = String::from_utf8(body)
            .map_err(|_| Error::Protocol("BMC returned non UTF-8 recording data".into()))?;
        check_status(&source).map_err(|error| AccessFailure {
            error,
            rejected: true,
        })?;
        Ok(source)
    }

    pub async fn recording_catalog(&self) -> Result<Vec<Entry>> {
        let path = if self.config.api_mode == ApiMode::Rest {
            "/api/logs/video"
        } else {
            "/rpc/getvideoinfo.asp"
        };
        parse_catalog(
            &self.recording_text(Method::GET, path, &[]).await?,
            self.config.api_mode,
        )
    }

    async fn recording_access(
        &self,
        file: &str,
        access: &str,
    ) -> std::result::Result<(), AccessFailure> {
        valid_file(file)?;
        let (method, path, query) = if self.config.api_mode == ApiMode::Rest {
            (
                Method::PUT,
                "/api/logs/video-log",
                [("file_name", file), ("file_access", access)],
            )
        } else {
            (
                Method::GET,
                "/rpc/downloadvideo.asp",
                [("FILE_NAME", file), ("FILE_ACCESS", access)],
            )
        };
        self.recording_text(method, path, &query).await?;
        Ok(())
    }

    /// Cancellation interrupts the transfer, but never skips unlocking the BMC.
    /// Publish only a complete, unlocked download, without replacing an existing file.
    pub async fn download_recording(
        &self,
        file: &str,
        destination: &Path,
        mut cancel: watch::Receiver<bool>,
        progress: impl Fn(u64, Option<u64>) + Send,
    ) -> Result<()> {
        valid_file(file)?;
        if *cancel.borrow() {
            return Err(Error::Invalid("录像下载已取消".into()));
        }
        if destination.try_exists()? {
            return Err(Error::Invalid("请选择一个尚不存在的录像文件名".into()));
        }
        let stage =
            tempfile::NamedTempFile::new_in(destination.parent().unwrap_or(Path::new(".")))?;
        tokio::select! {
            biased;
            _ = cancel.changed() => return Err(Error::Invalid("录像下载已取消".into())),
            result = self.recording_access(file, "4") => result?,
        }
        // Do not cancel a lock request whose result is uncertain: wait for its bounded
        // HTTP result, then release a successfully acquired lock even if canceled.
        if let Err(failure) = self.recording_access(file, "1").await {
            if !failure.rejected {
                let release =
                    tokio::time::timeout(Duration::from_secs(10), self.recording_access(file, "0"))
                        .await;
                if !matches!(release, Ok(Ok(()))) {
                    return Err(Error::Protocol(format!(
                        "加锁结果未知：{failure}；BMC 录像解锁未成功"
                    )));
                }
            }
            return Err(failure.error);
        }
        let result = tokio::select! {
            biased;
            _ = async { if !*cancel.borrow() { let _ = cancel.changed().await; } } => Err(Error::Invalid("录像下载已取消".into())),
            result = async {
                let client = reqwest::Client::builder()
                    .danger_accept_invalid_certs(self.server.trust_invalid_certificate)
                    .danger_accept_invalid_hostnames(self.server.trust_invalid_certificate)
                    .connect_timeout(Duration::from_secs(10)).read_timeout(Duration::from_secs(30))
                    .redirect(reqwest::redirect::Policy::none()).build()?;
                let mut url = url::Url::parse(&self.server.origin()).map_err(|e| Error::Invalid(e.to_string()))?;
                if self.config.api_mode == ApiMode::Rest {
                    url.set_path("/api/logs/video-data");
                    url.query_pairs_mut().append_pair("file", file);
                } else {
                    url.set_path("/video/");
                    url.path_segments_mut().map_err(|_| Error::Invalid("Invalid BMC URL".into()))?
                        .pop_if_empty().extend(file.trim_start_matches('/').split('/'));
                }
                let mut response = self.recording_request(&client, Method::GET, url).send().await?;
                if !response.status().is_success() {
                    return Err(Error::Protocol(format!("BMC recording download: HTTP {}", response.status().as_u16())));
                }
                let total = response.content_length();
                let mut output = tokio::fs::File::from_std(stage.reopen()?);
                let mut downloaded = 0_u64;
                progress(0, total);
                while let Some(chunk) = response.chunk().await? {
                    output.write_all(&chunk).await?;
                    downloaded = downloaded.checked_add(chunk.len() as u64)
                        .ok_or_else(|| Error::Protocol("Recording download is too large".into()))?;
                    progress(downloaded, total);
                }
                if total.is_some_and(|expected| expected != downloaded) || downloaded == 0 {
                    return Err(Error::Protocol("BMC recording download is incomplete".into()));
                }
                output.sync_all().await?;
                Ok(())
            } => result,
        };
        let unlocked =
            tokio::time::timeout(Duration::from_secs(10), self.recording_access(file, "0")).await;
        match unlocked {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(Error::Protocol(format!(
                    "{}；BMC 录像解锁失败：{error}",
                    result
                        .as_ref()
                        .err()
                        .map_or("下载完成".into(), ToString::to_string)
                )));
            }
            Err(_) => return Err(Error::Timeout("BMC recording unlock")),
        }
        result?;
        stage
            .persist_noclobber(destination)
            .map_err(|e| Error::Io(e.error))?;
        Ok(())
    }
}
