use crate::{
    Error, Result,
    auth::WebSession,
    protocol::{self, Header},
};
use std::{
    net::SocketAddr,
    pin::Pin,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

pub mod identity;
mod single_port;

pub trait AsyncSocket: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncSocket for T {}
pub type Socket = Pin<Box<dyn AsyncSocket>>;

pub struct Connection {
    pub stream: Socket,
    pub local_address: SocketAddr,
}

impl WebSession {
    pub async fn open_video(&self) -> Result<Connection> {
        if !self.config.kvm_enabled {
            return Err(Error::Invalid(
                "KVM redirection is disabled on this BMC".into(),
            ));
        }
        if self.config.privileges & 1 == 0 {
            return Err(Error::Authentication(
                "This account has no KVM privilege".into(),
            ));
        }
        self.open_channel(self.config.kvm_port, self.config.kvm_secure, "VIDEO")
            .await
    }

    pub async fn open_channel(&self, port: u16, secure: bool, channel: &str) -> Result<Connection> {
        self.open_channel_with_mode(port, secure, channel, self.config.single_port)
            .await
    }

    pub(crate) async fn open_channel_with_mode(
        &self,
        port: u16,
        secure: bool,
        channel: &str,
        single_port: bool,
    ) -> Result<Connection> {
        if !["VIDEO", "CDMEDIA", "HDMEDIA"].contains(&channel) {
            return Err(Error::Invalid("Invalid channel".into()));
        }
        let tcp = timeout(
            Duration::from_secs(10),
            TcpStream::connect((self.server.host.as_str(), port)),
        )
        .await
        .map_err(|_| Error::Timeout("BMC socket connection"))??;
        tcp.set_nodelay(true)?;
        let local_address = tcp.local_addr()?;
        let mut stream: Socket = if secure {
            let connector = native_tls::TlsConnector::builder()
                .danger_accept_invalid_certs(self.server.trust_invalid_certificate)
                .danger_accept_invalid_hostnames(self.server.trust_invalid_certificate)
                .build()?;
            let tls = timeout(
                Duration::from_secs(10),
                tokio_native_tls::TlsConnector::from(connector).connect(&self.server.host, tcp),
            )
            .await
            .map_err(|_| Error::Timeout("BMC TLS handshake"))??;
            Box::pin(tls)
        } else {
            Box::pin(tcp)
        };
        if single_port {
            let host = if self.server.host.contains(':') {
                format!("[{}]", self.server.host)
            } else {
                self.server.host.clone()
            };
            let scheme = if secure { "HTTPS" } else { "HTTP" };
            let handshake = format!(
                "CONNECT {host}:{} {scheme}/1.1\r\n cookie {}\r\n\r\nJVIEWER {channel} cookie {}\r\n\r\n",
                self.server.web_port, self.cookie, self.cookie
            );
            timeout(Duration::from_secs(10), async {
                stream.write_all(handshake.as_bytes()).await?;
                stream.flush().await
            })
            .await
            .map_err(|_| Error::Timeout("Single-port handshake write"))??;
            if single_port::confirmation(&mut stream).await? {
                stream = Box::pin(single_port::HttpStream {
                    stream,
                    first_read: true,
                });
            }
        }
        Ok(Connection {
            stream,
            local_address,
        })
    }
}

pub struct Packet {
    pub header: Header,
    pub body: Vec<u8>,
    pub wire_bytes: u64,
    pub bandwidth: Option<BandwidthMeasurement>,
}

pub struct BandwidthMeasurement {
    pub bytes: u64,
    pub elapsed: Duration,
}

impl BandwidthMeasurement {
    pub fn bytes_per_second(&self) -> f64 {
        self.bytes as f64 / self.elapsed.as_secs_f64().max(0.001)
    }
    pub fn preset(&self) -> u32 {
        let rate = self.bytes_per_second();
        if rate < 49_152.0 {
            32_768
        } else if rate < 98_304.0 {
            65_536
        } else if rate < 720_896.0 {
            131_072
        } else if rate < 7_208_960.0 {
            1_310_720
        } else {
            13_107_200
        }
    }
}

pub async fn write_packet<W: AsyncWrite + Unpin>(stream: &mut W, bytes: &[u8]) -> Result<()> {
    timeout(Duration::from_secs(10), async {
        stream.write_all(bytes).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| Error::Timeout("BMC socket write"))??;
    Ok(())
}

pub async fn read_packet<R: AsyncRead + Unpin>(stream: &mut R) -> Result<Packet> {
    let mut bytes = [0; protocol::HEADER_LEN];
    timeout(Duration::from_secs(30), stream.read_exact(&mut bytes))
        .await
        .map_err(|_| Error::Timeout("BMC packet header"))??;
    let header = Header::parse(&bytes)?;
    if header.kind == 17 {
        // One header advertises the block size and repeat count. The following
        // blocks have no individual IVTP headers (NullReader, checked in the JAR).
        let count = u64::from(header.status.max(1));
        let total = u64::from(header.length) * count;
        if header.length == 0 || header.length > 262_144 || total > protocol::MAX_PACKET as u64 {
            return Err(Error::Protocol("Invalid bandwidth measurement size".into()));
        }
        let started = Instant::now();
        let mut block = vec![0; header.length as usize];
        for _ in 0..count {
            timeout(Duration::from_secs(30), stream.read_exact(&mut block))
                .await
                .map_err(|_| Error::Timeout("BMC bandwidth data"))??;
        }
        return Ok(Packet {
            header,
            body: vec![],
            wire_bytes: total + protocol::HEADER_LEN as u64,
            bandwidth: Some(BandwidthMeasurement {
                bytes: total,
                elapsed: started.elapsed(),
            }),
        });
    }
    let mut body = vec![0; header.length as usize];
    timeout(Duration::from_secs(30), stream.read_exact(&mut body))
        .await
        .map_err(|_| Error::Timeout("BMC packet body"))??;
    Ok(Packet {
        header,
        wire_bytes: body.len() as u64 + protocol::HEADER_LEN as u64,
        body,
        bandwidth: None,
    })
}
