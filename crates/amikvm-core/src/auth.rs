//! Authentication and capability discovery, traced to StandAloneConnectionDialog.
//! Legacy RPC responses are JavaScript data, never executed by this client.
use crate::{
    Error, Result,
    server::{ApiMode, Server},
};
use reqwest::{Client, Method, StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfig {
    pub api_mode: ApiMode,
    pub privileges: u32,
    pub kvm_port: u16,
    pub kvm_secure: bool,
    pub kvm_enabled: bool,
    pub single_port: bool,
    pub media_secure: bool,
    pub cd_port: Option<u16>,
    pub hd_port: Option<u16>,
    pub cd_instances: u8,
    pub hd_instances: u8,
    pub kvm_cd_instances: u8,
    pub kvm_hd_instances: u8,
    pub cd_enabled: bool,
    pub hd_enabled: bool,
    pub kvm_license: Option<u8>,
    pub media_license: Option<u8>,
    pub keyboard_layout: String,
    pub oem_features: u64,
    pub retry_count: u32,
    pub retry_interval: u32,
    pub power_save_mode: u8,
}

pub struct WebSession {
    pub server: Server,
    pub config: SessionConfig,
    pub(crate) client: Client,
    pub(crate) cookie: String,
    pub(crate) csrf: Option<String>,
    pub(crate) token: String,
}

struct ResponseData {
    status: StatusCode,
    cookie: Option<String>,
    fields: Fields,
}

pub struct Fields {
    source: String,
    json: Option<Value>,
}

impl Fields {
    pub fn parse(source: String) -> Self {
        let json = serde_json::from_str(&source).ok();
        Self { source, json }
    }

    pub fn text(&self, key: &str) -> Option<String> {
        fn find<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
            match value {
                Value::Object(values) => values
                    .get(key)
                    .or_else(|| values.values().find_map(|v| find(v, key))),
                Value::Array(values) => values.iter().find_map(|v| find(v, key)),
                _ => None,
            }
        }
        if let Some(value) = self.json.as_ref().and_then(|v| find(v, key)) {
            return match value {
                Value::String(s) => Some(s.clone()),
                Value::Null => None,
                _ => Some(value.to_string()),
            };
        }
        let pattern = format!(r#"(?:^|[\s{{,])['"]?{}['"]?\s*[:=]\s*"#, regex::escape(key));
        let regex = regex::Regex::new(&pattern).ok()?;
        let found = regex.find(&self.source)?;
        let rest = &self.source[found.end()..];
        if let Some(quote @ ('\'' | '"')) = rest.chars().next() {
            let mut escaped = false;
            let mut value = String::new();
            for c in rest[1..].chars() {
                if escaped {
                    value.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == quote {
                    return Some(value);
                } else {
                    value.push(c);
                }
            }
            None
        } else {
            let value = rest.split([',', '}', ';', '\r', '\n']).next()?.trim();
            (!value.is_empty()).then(|| value.to_owned())
        }
    }

    pub fn number(&self, key: &str) -> Option<u64> {
        let s = self.text(key)?;
        match s.as_str() {
            "true" => Some(1),
            "false" => Some(0),
            _ if s.starts_with("0x") => u64::from_str_radix(&s[2..], 16).ok(),
            _ => s.parse().ok(),
        }
    }

    fn required(&self, key: &str) -> Result<String> {
        self.text(key)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Protocol(format!("Missing {key} in BMC response")))
    }

    fn checked(&self) -> Result<()> {
        if self
            .text("HAPI_STATUS")
            .and_then(|s| s.parse::<i64>().ok())
            .is_some_and(|v| v < 0)
        {
            return Err(Error::Authentication("BMC rejected the RPC request".into()));
        }
        Ok(())
    }
}

fn checked_u16(fields: &Fields, key: &str) -> Result<u16> {
    fields
        .number(key)
        .and_then(|v| u16::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| Error::Protocol(format!("Invalid or missing {key}")))
}

fn count(fields: &Fields, key: &str) -> u8 {
    fields
        .number(key)
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(0)
}

impl WebSession {
    pub fn with_config(&self, config: SessionConfig) -> Self {
        Self {
            server: self.server.clone(),
            config,
            client: self.client.clone(),
            cookie: self.cookie.clone(),
            csrf: self.csrf.clone(),
            token: self.token.clone(),
        }
    }
    pub fn protect_diagnostics(&self, recorder: &crate::diagnostics::Recorder) {
        recorder.protect(&self.token);
        if let Some(csrf) = &self.csrf {
            recorder.protect(csrf);
        }
        recorder.protect(&self.cookie);
        for cookie in self.cookie.split(';') {
            if let Some((_, value)) = cookie.split_once('=') {
                recorder.protect(value.trim());
            }
        }
    }
    pub fn input_cipher(&self) -> Result<crate::input::encryption::Cipher> {
        crate::input::encryption::Cipher::from_token(&self.token)
    }
    pub fn authentication_packet(
        &self,
        local_ip: &str,
        local_name: &str,
        mac: &str,
    ) -> Result<Vec<u8>> {
        crate::protocol::authenticate(&self.token, local_ip, local_name, mac, &self.server.host)
    }

    pub fn cookie_packet(&self) -> Result<Vec<u8>> {
        crate::protocol::packet(21, 0, self.cookie.as_bytes())
    }

    pub fn reconnect_packet(
        &self,
        local_ip: &str,
        local_name: &str,
        mac: &str,
        session_id: u8,
    ) -> Result<Vec<u8>> {
        crate::protocol::reconnect(&self.token, local_ip, local_name, mac, session_id)
    }

    pub async fn login(server: Server, password: &str) -> Result<Self> {
        Self::login_with_discovery(server, password, true).await
    }

    /// Captured images do not require a normal KVM token or media discovery.
    /// Preview channel settings are discovered on demand; BSOD is HTTP only.
    pub async fn login_web(server: Server, password: &str) -> Result<Self> {
        Self::login_with_discovery(server, password, false).await
    }

    async fn login_with_discovery(server: Server, password: &str, discover: bool) -> Result<Self> {
        if password.is_empty() {
            return Err(Error::Invalid("Password is required".into()));
        }
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let client = Client::builder()
            .cookie_provider(jar)
            .danger_accept_invalid_certs(server.trust_invalid_certificate)
            .danger_accept_invalid_hostnames(server.trust_invalid_certificate)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("AMIKVM/0.1")
            .build()?;
        let mode = match server.api_mode {
            ApiMode::Auto => {
                let response =
                    Self::unauthenticated(&client, &server, ApiMode::Rest, password).await?;
                if response.status == StatusCode::NOT_FOUND
                    || response.status == StatusCode::METHOD_NOT_ALLOWED
                {
                    Self::establish(client, server, ApiMode::Rpc, password, None, discover).await?
                } else {
                    Self::establish(
                        client,
                        server,
                        ApiMode::Rest,
                        password,
                        Some(response),
                        discover,
                    )
                    .await?
                }
            }
            mode => Self::establish(client, server, mode, password, None, discover).await?,
        };
        Ok(mode)
    }

    async fn unauthenticated(
        client: &Client,
        server: &Server,
        mode: ApiMode,
        password: &str,
    ) -> Result<ResponseData> {
        let (method, path, query) = match mode {
            ApiMode::Rpc => (
                Method::GET,
                "/rpc/WEBSES/create.asp",
                vec![
                    ("WEBVAR_USERNAME", server.username.as_str()),
                    ("WEBVAR_PASSWORD", password),
                ],
            ),
            _ => (
                Method::POST,
                "/api/session",
                vec![
                    ("username", server.username.as_str()),
                    ("password", password),
                ],
            ),
        };
        let response = client
            .request(method, format!("{}{path}", server.origin()))
            .query(&query)
            .header(header::CONTENT_LENGTH, 0)
            .send()
            .await?;
        let status = response.status();
        let cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|v| v.split(';').next())
            .find(|v| v.starts_with("QSESSIONID="))
            .map(str::to_owned);
        let fields = Fields::parse(response.text().await?);
        Ok(ResponseData {
            status,
            cookie,
            fields,
        })
    }

    async fn establish(
        client: Client,
        server: Server,
        mode: ApiMode,
        password: &str,
        response: Option<ResponseData>,
        discover: bool,
    ) -> Result<Self> {
        let response = match response {
            Some(r) => r,
            None => Self::unauthenticated(&client, &server, mode, password).await?,
        };
        if !response.status.is_success() {
            return Err(Error::Authentication(format!(
                "BMC returned HTTP {}",
                response.status.as_u16()
            )));
        }
        response.fields.checked()?;
        let cookie = response
            .cookie
            .or_else(|| {
                response.fields.text("SESSION_COOKIE").map(|cookie| {
                    if cookie.starts_with("QSESSIONID=") {
                        cookie
                    } else {
                        format!("QSESSIONID={cookie}")
                    }
                })
            })
            .ok_or_else(|| Error::Authentication("BMC did not return a session cookie".into()))?;
        if cookie.contains(['\r', '\n']) {
            return Err(Error::Protocol("Invalid session cookie".into()));
        }
        let csrf = response.fields.text("CSRFToken");
        let mut session = Self {
            server,
            client,
            cookie,
            csrf,
            token: String::new(),
            config: SessionConfig {
                api_mode: mode,
                privileges: 0,
                kvm_port: 7578,
                kvm_secure: false,
                kvm_enabled: discover,
                single_port: false,
                media_secure: false,
                cd_port: None,
                hd_port: None,
                cd_instances: 0,
                hd_instances: 0,
                kvm_cd_instances: 0,
                kvm_hd_instances: 0,
                cd_enabled: false,
                hd_enabled: false,
                kvm_license: None,
                media_license: None,
                keyboard_layout: "AD".into(),
                oem_features: 0,
                retry_count: 3,
                retry_interval: 15,
                power_save_mode: 0,
            },
        };
        if discover {
            if let Err(error) = session.discover(response.fields).await {
                let _ = session.logout().await;
                return Err(error);
            }
        }
        Ok(session)
    }

    pub(crate) fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.server.origin()))
            .header(header::COOKIE, self.http_cookie());
        if let Some(token) = &self.csrf {
            request = request.header("X-CSRFTOKEN", token);
        }
        request
    }

    pub(crate) fn http_cookie(&self) -> String {
        if self.config.api_mode == ApiMode::Rpc {
            format!(
                "SessionCookie={}",
                self.cookie
                    .strip_prefix("QSESSIONID=")
                    .or_else(|| self.cookie.strip_prefix("SessionCookie="))
                    .unwrap_or(&self.cookie)
            )
        } else {
            self.cookie.clone()
        }
    }

    pub async fn get_fields(&self, path: &str) -> Result<Fields> {
        let response = self.request(Method::GET, path).send().await?;
        if !response.status().is_success() {
            return Err(Error::Protocol(format!(
                "{path}: HTTP {}",
                response.status().as_u16()
            )));
        }
        let fields = Fields::parse(response.text().await?);
        fields.checked()?;
        Ok(fields)
    }

    async fn discover(&mut self, login: Fields) -> Result<()> {
        let rest = self.config.api_mode == ApiMode::Rest;
        let privileges = if rest {
            login
        } else {
            self.get_fields("/rpc/getrole.asp").await?
        };
        let privilege_key = if rest {
            "extendedpriv"
        } else {
            "EXTENDED_PRIV"
        };
        self.config.privileges = privileges
            .number(privilege_key)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| Error::Protocol("Missing user privileges".into()))?;
        let adviser = self
            .get_fields(if rest {
                "/api/settings/media/adviser"
            } else {
                "/rpc/getadvisercfg.asp"
            })
            .await?;
        let media = self
            .get_fields(if rest {
                "/api/settings/media/instance"
            } else {
                "/rpc/getvmediacfg.asp"
            })
            .await?;
        let key = |r: &'static str, l: &'static str| if rest { r } else { l };
        self.config.kvm_enabled = adviser.number(key("status", "V_STR_KVM_STATUS")) != Some(0);
        self.config.single_port =
            media.number(key("single_port_enabled", "V_SINGLE_PORT_ENABLED")) == Some(1);
        self.config.kvm_secure = if self.config.single_port {
            self.server.https
        } else {
            adviser.number(key("secure_channel", "V_STR_SECURE_CHANNEL")) == Some(1)
        };
        self.config.kvm_port = checked_u16(
            &adviser,
            if self.config.single_port {
                key("web_port", "V_STR_WEB_PORT")
            } else {
                key("kvm_port", "V_STR_KVM_PORT")
            },
        )?;
        self.config.media_secure = if self.config.single_port {
            self.server.https
        } else {
            media.number(key("secure_channel", "V_STR_SECURE_CHANNEL")) == Some(1)
        };
        self.config.oem_features = if rest {
            media.number("oemFeature")
        } else {
            adviser.number("V_STR_OEM_FEATURE_STATUS")
        }
        .unwrap_or(0);
        self.config.keyboard_layout = adviser
            .text(key("keyboard_layout", "V_STR_KEYBOARD_LAYOUT"))
            .unwrap_or_else(|| "AD".into());
        self.config.kvm_license = adviser
            .number(key("license", "V_STR_KVM_LICENSE_STATUS"))
            .and_then(|v| u8::try_from(v).ok());
        self.config.media_license = media
            .number(key("license", "V_MEDIA_LICENSE_STATUS"))
            .and_then(|v| u8::try_from(v).ok());
        self.config.cd_instances = count(&media, key("num_cd", "V_NUM_CD"));
        self.config.hd_instances = count(&media, key("num_hd", "V_NUM_HD"));
        self.config.kvm_cd_instances = count(&media, key("kvm_num_cd", "V_KVM_NUM_CD"));
        self.config.kvm_hd_instances = count(&media, key("kvm_num_hd", "V_KVM_NUM_HD"));
        self.config.cd_enabled = media.number(key("cd_status", "V_CD_STATUS")) == Some(1);
        self.config.hd_enabled = media.number(key("hd_status", "V_HD_STATUS")) == Some(1);
        self.config.power_save_mode = count(&media, key("power_save_mode", "V_POWER_SAVE_MODE"));
        if !self.config.single_port {
            let secure = self.config.media_secure;
            self.config.cd_port = checked_u16(
                &media,
                if secure {
                    key("cd_secure_port", "V_STR_CD_SECURE_PORT")
                } else {
                    key("cd_port", "V_STR_CD_PORT")
                },
            )
            .ok();
            self.config.hd_port = checked_u16(
                &media,
                if secure {
                    key("hd_secure_port", "V_STR_HD_SECURE_PORT")
                } else {
                    key("hd_port", "V_STR_HD_PORT")
                },
            )
            .ok();
        }
        if self.config.oem_features & 32 != 0 {
            self.config.retry_count = adviser
                .number(key("retry_count", "V_STR_RETRY_COUNT"))
                .unwrap_or(3)
                .min(100) as u32;
            self.config.retry_interval = adviser
                .number(key("retry_interval", "V_STR_RETRY_INTERVAL"))
                .unwrap_or(15)
                .min(3600) as u32;
        }
        let token = self
            .get_fields(if rest {
                "/api/kvm/token"
            } else {
                "/rpc/getsessiontoken.asp"
            })
            .await?;
        self.token = token.required(if rest { "token" } else { "SESSION_TOKEN" })?;
        Ok(())
    }

    pub async fn logout(&self) -> Result<()> {
        let (method, path) = if self.config.api_mode == ApiMode::Rest {
            (Method::DELETE, "/api/session")
        } else {
            (Method::GET, "/rpc/WEBSES/logout.asp")
        };
        self.request(method, path)
            .header(header::CONTENT_LENGTH, 0)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}
