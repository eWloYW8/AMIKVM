//! Local application preferences. A failed write never changes the active value.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{io::Write, path::PathBuf};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "zh-CN")]
    Chinese,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "fr")]
    French,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Self::Chinese => "zh-CN",
            Self::English => "en",
            Self::French => "fr",
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Data {
    #[serde(default)]
    language: Language,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

pub struct Store {
    path: PathBuf,
    data: Data,
}

impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self { path, data })
    }

    pub fn language(&self) -> Language {
        self.data.language
    }

    pub fn set_language(&mut self, language: Language) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Error::Invalid("Invalid preferences path".into()))?;
        std::fs::create_dir_all(parent)?;
        let data = Data {
            language,
            extra: self.data.extra.clone(),
        };
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&serde_json::to_vec_pretty(&data)?)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        self.data = data;
        Ok(())
    }
}
