use anyhow::Result;
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use quinn::{MtuDiscoveryConfig, TransportConfig, VarInt};
use rustls::{Certificate, RootCertStore};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::constants::{
    QUIC_MTU_OVERHEAD, QUINCY_CIPHER_SUITES, TLS_ALPN_PROTOCOLS, TLS_PROTOCOL_VERSIONS,
};
use crate::utils::certificates::{load_certificates_from_file, load_private_key_from_file};
use tracing::{error, warn};

/// Represents the configuration for a Quincy server.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ServerConfig {
    tunnel_path: Option<PathBuf>,
    /// Configuration for the tunnels associated with this server
    pub tunnels: HashMap<String, TunnelConfig>,
    /// Miscellaneous connection configuration
    pub connection: ConnectionConfig,
    /// Logging configuration
    pub log: LogConfig,
}

/// Represents the configuration for a Quincy tunnel.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TunnelConfig {
    /// The name of the tunnel
    pub name: String,
    /// The certificate to use for the tunnel
    pub certificate_file: PathBuf,
    /// The certificate private key to use for the tunnel
    pub certificate_key_file: PathBuf,
    /// The address to bind the tunnel to
    #[serde(default = "default_bind_address")]
    pub bind_address: Ipv4Addr,
    /// The port to bind the tunnel to
    #[serde(default = "default_bind_port")]
    pub bind_port: u16,
    /// The address of this tunnel
    pub address_tunnel: Ipv4Addr,
    /// The address mask for this tunnel
    pub address_mask: Ipv4Addr,
    /// A path to a file containing a list of users and their password hashes
    pub users_file: PathBuf,
    #[serde(default = "default_auth_timeout")]
    /// The amount of time in seconds to wait for authentication before closing the connection
    pub auth_timeout: u32,
}

/// Represents the configuration for a Quincy client.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ClientConfig {
    /// Connection string to be used to connect to a Quincy server
    pub connection_string: String,
    /// Authentication configuration
    pub authentication: ClientAuthenticationConfig,
    /// Miscellaneous connection configuration
    pub connection: ConnectionConfig,
    /// Logging configuration
    pub log: LogConfig,
}

/// Represents the configuration for a Quincy client's authentication.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ClientAuthenticationConfig {
    /// The username to use for authentication
    pub username: String,
    /// The password to use for authentication
    pub password: String,
    /// A list of trusted certificates
    pub trusted_certificates: Vec<PathBuf>,
    /// The interval at which to send the session token
    #[serde(default = "default_auth_timeout")]
    pub auth_interval: u32,
}

/// Represents miscellaneous connection configuration.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ConnectionConfig {
    /// The MTU to use for connections and the TUN interface
    pub mtu: u32,
    /// The size of the send buffer of the socket and Quinn endpoint
    #[serde(default = "default_buffer_size")]
    pub send_buffer_size: u64,
    /// The size of the receive buffer of the socket and Quinn endpoint
    #[serde(default = "default_buffer_size")]
    pub recv_buffer_size: u64,
}

/// Represents logging configuration.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct LogConfig {
    /// The log level to use
    #[serde(default = "default_log_level")]
    pub level: String,
}

pub trait ConfigInit<T: DeserializeOwned> {
    /// Initializes the configuration object from the given Figment
    ///
    /// ### Arguments
    /// - `figment` - the Figment to use for initialization
    fn init(figment: Figment, _env_prefix: &str) -> Result<T> {
        Ok(figment.extract()?)
    }
}

pub trait FromPath<T: DeserializeOwned + ConfigInit<T>> {
    /// Creates a configuration object from the given path and ENV prefix
    ///
    /// ### Arguments
    /// - `path` - a path to the configuration file
    /// - `env_prefix` - the ENV prefix to use for overrides
    fn from_path(path: &PathBuf, env_prefix: &str) -> Result<T> {
        let figment = Figment::new()
            .merge(Toml::file(path))
            .merge(Env::prefixed(env_prefix));

        T::init(figment, env_prefix)
    }
}

impl ConfigInit<ServerConfig> for ServerConfig {
    fn init(figment: Figment, env_prefix: &str) -> Result<Self> {
        let mut config: ServerConfig = figment.extract()?;

        let tunnel_configs: Vec<TunnelConfig> = match &config.tunnel_path {
            Some(tunnel_path) => {
                if tunnel_path.is_dir() {
                    tunnel_path
                        .read_dir()?
                        .flatten()
                        .filter_map(|config_file| {
                            TunnelConfig::from_path(&config_file.path(), env_prefix).ok()
                        })
                        .collect()
                } else {
                    warn!("Failed to load tunnel configuration files from '{tunnel_path:?}' - the folder does not exist");
                    vec![]
                }
            }
            None => vec![],
        };

        for tunnel in tunnel_configs {
            match config.tunnels.entry(tunnel.name.clone()) {
                Entry::Occupied(_) => warn!("Tunnel with the name {} already exists", tunnel.name),
                Entry::Vacant(slot) => {
                    slot.insert(tunnel);
                }
            }
        }

        Ok(config)
    }
}
impl ConfigInit<ClientConfig> for ClientConfig {}
impl ConfigInit<TunnelConfig> for TunnelConfig {}

impl FromPath<ServerConfig> for ServerConfig {}
impl FromPath<ClientConfig> for ClientConfig {}
impl FromPath<TunnelConfig> for TunnelConfig {}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_bind_address() -> Ipv4Addr {
    "0.0.0.0".parse().expect("Default address is valid")
}

fn default_bind_port() -> u16 {
    55555
}

fn default_buffer_size() -> u64 {
    2097152
}

fn default_auth_timeout() -> u32 {
    120
}

impl ClientConfig {
    /// Creates Quinn client configuration from this Quincy client configuration.
    ///
    /// ### Returns
    /// - `quinn::ClientConfig` - the Quinn client configuration
    pub fn as_quinn_client_config(&self) -> Result<quinn::ClientConfig> {
        let trusted_certificates: Vec<Certificate> = self
            .authentication
            .trusted_certificates
            .iter()
            .filter_map(|cert_path| match load_certificates_from_file(cert_path) {
                Ok(certificates) => Some(certificates),
                Err(e) => {
                    error!("Could not load certificates from {cert_path:?} due to an error: {e}");
                    None
                }
            })
            .flatten()
            .collect();

        let mut cert_store = RootCertStore::empty();

        for certificate in trusted_certificates {
            cert_store.add(&certificate)?;
        }

        let mut rustls_config = rustls::ClientConfig::builder()
            .with_cipher_suites(QUINCY_CIPHER_SUITES)
            .with_safe_default_kx_groups()
            .with_protocol_versions(TLS_PROTOCOL_VERSIONS)?
            .with_root_certificates(cert_store)
            .with_no_client_auth();

        rustls_config.alpn_protocols = TLS_ALPN_PROTOCOLS.clone();

        let mut quinn_config = quinn::ClientConfig::new(Arc::new(rustls_config));
        let mut transport_config = TransportConfig::default();
        let mut mtu_config = MtuDiscoveryConfig::default();

        transport_config.max_idle_timeout(Some(
            VarInt::from_u32(self.authentication.auth_interval * 2 * 1_000).into(),
        ));

        mtu_config.upper_bound(self.connection.mtu as u16 + QUIC_MTU_OVERHEAD);

        transport_config.mtu_discovery_config(Some(mtu_config));
        quinn_config.transport_config(Arc::new(transport_config));

        Ok(quinn_config)
    }
}

impl TunnelConfig {
    /// Creates Quinn server configuration from this Quincy tunnel configuration.
    ///
    /// ### Arguments
    /// - `connection_config` - the connection configuration to use
    ///
    /// ### Returns
    /// - `quinn::ServerConfig` - the Quinn server configuration
    pub fn as_quinn_server_config(
        &self,
        connection_config: &ConnectionConfig,
    ) -> Result<quinn::ServerConfig> {
        let certificate_file_path = self.certificate_file.clone();
        let certificate_key_path = self.certificate_key_file.clone();
        let key = load_private_key_from_file(&certificate_key_path)?;
        let certs = load_certificates_from_file(&certificate_file_path)?;

        let mut rustls_config = rustls::ServerConfig::builder()
            .with_cipher_suites(QUINCY_CIPHER_SUITES)
            .with_safe_default_kx_groups()
            .with_protocol_versions(TLS_PROTOCOL_VERSIONS)?
            .with_no_client_auth()
            .with_single_cert(certs, key)?;

        rustls_config.alpn_protocols = TLS_ALPN_PROTOCOLS.clone();

        let mut quinn_config = quinn::ServerConfig::with_crypto(Arc::new(rustls_config));
        let mut transport_config = TransportConfig::default();
        let mut mtu_config = MtuDiscoveryConfig::default();

        transport_config
            .max_idle_timeout(Some(VarInt::from_u32(self.auth_timeout * 2 * 1_000).into()));

        mtu_config.upper_bound(connection_config.mtu as u16 + QUIC_MTU_OVERHEAD);

        transport_config.mtu_discovery_config(Some(mtu_config));
        quinn_config.transport_config(Arc::new(transport_config));

        Ok(quinn_config)
    }
}
