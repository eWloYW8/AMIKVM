use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiMode {
    #[default]
    Auto,
    Rest,
    Rpc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: Uuid,
    pub name: String,
    pub host: String,
    pub web_port: u16,
    pub https: bool,
    pub username: String,
    pub api_mode: ApiMode,
    pub trust_invalid_certificate: bool,
    pub favorite: bool,
    pub tags: Vec<String>,
    pub notes: String,
    pub credential_saved: bool,
    pub updated_at: u64,
    pub last_connected_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInput {
    pub id: Option<Uuid>,
    pub name: String,
    pub host: String,
    pub web_port: u16,
    pub https: bool,
    pub username: String,
    pub api_mode: ApiMode,
    pub trust_invalid_certificate: bool,
    pub favorite: bool,
    pub tags: Vec<String>,
    pub notes: String,
}

impl ServerInput {
    pub fn validate(&mut self) -> Result<()> {
        self.name = self.name.trim().to_owned();
        self.host = self
            .host
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        self.username = self.username.trim().to_owned();
        if self.name.is_empty() || self.name.len() > 160 {
            return Err(Error::Invalid(
                "Server name must contain 1–160 bytes".into(),
            ));
        }
        if self.host.is_empty()
            || self.host.contains(['/', '\\', '?', '#', '@'])
            || self.host.chars().any(char::is_whitespace)
        {
            return Err(Error::Invalid(
                "Enter a hostname or IP address without a URL scheme or path".into(),
            ));
        }
        let authority = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        url::Url::parse(&format!("https://{authority}:{}", self.web_port))
            .map_err(|_| Error::Invalid("Invalid hostname or IP address".into()))?;
        if self.web_port == 0 || self.username.is_empty() || self.username.len() > 128 {
            return Err(Error::Invalid(
                "A valid web port and username are required".into(),
            ));
        }
        self.tags = self
            .tags
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        self.tags.sort();
        self.tags.dedup();
        Ok(())
    }
}

impl Server {
    pub fn origin(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!(
            "{}://{host}:{}",
            if self.https { "https" } else { "http" },
            self.web_port
        )
    }
}

impl From<&Server> for ServerInput {
    fn from(s: &Server) -> Self {
        Self {
            id: Some(s.id),
            name: s.name.clone(),
            host: s.host.clone(),
            web_port: s.web_port,
            https: s.https,
            username: s.username.clone(),
            api_mode: s.api_mode,
            trust_invalid_certificate: s.trust_invalid_certificate,
            favorite: s.favorite,
            tags: s.tags.clone(),
            notes: s.notes.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct StoreData {
    version: u32,
    servers: Vec<Server>,
}

pub struct ServerStore {
    path: PathBuf,
    data: StoreData,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl ServerStore {
    pub fn open(path: PathBuf) -> Result<Self> {
        let data = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<StoreData>(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoreData {
                version: 1,
                ..Default::default()
            },
            Err(error) => return Err(error.into()),
        };
        if data.version != 1 {
            return Err(Error::Invalid("Unsupported server database version".into()));
        }
        Ok(Self { path, data })
    }

    pub fn list(&self) -> Vec<Server> {
        self.data.servers.clone()
    }

    pub fn get(&self, id: Uuid) -> Result<Server> {
        self.data
            .servers
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| Error::Invalid("Server not found".into()))
    }

    pub fn save(&mut self, mut input: ServerInput, credential_saved: bool) -> Result<Server> {
        input.validate()?;
        let previous = input
            .id
            .and_then(|id| self.data.servers.iter().find(|s| s.id == id).cloned());
        let server = Server {
            id: input.id.unwrap_or_else(Uuid::new_v4),
            name: input.name,
            host: input.host,
            web_port: input.web_port,
            https: input.https,
            username: input.username,
            api_mode: input.api_mode,
            trust_invalid_certificate: input.trust_invalid_certificate,
            favorite: input.favorite,
            tags: input.tags,
            notes: input.notes,
            credential_saved,
            updated_at: now(),
            last_connected_at: previous.and_then(|s| s.last_connected_at),
        };
        let mut next = self.data.servers.clone();
        if let Some(existing) = next.iter_mut().find(|s| s.id == server.id) {
            *existing = server.clone();
        } else {
            next.push(server.clone());
        }
        self.persist(next)?;
        Ok(server)
    }

    pub fn remove(&mut self, id: Uuid) -> Result<()> {
        self.get(id)?;
        self.persist(
            self.data
                .servers
                .iter()
                .filter(|s| s.id != id)
                .cloned()
                .collect(),
        )
    }

    pub fn connected(&mut self, id: Uuid) -> Result<()> {
        let mut next = self.data.servers.clone();
        let server = next
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| Error::Invalid("Server not found".into()))?;
        server.last_connected_at = Some(now());
        self.persist(next)
    }

    fn persist(&mut self, servers: Vec<Server>) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Error::Invalid("Invalid server database path".into()))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".servers-{}.tmp", Uuid::new_v4()));
        let data = StoreData {
            version: 1,
            servers,
        };
        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(&data)?)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        self.data = data;
        Ok(())
    }
}
