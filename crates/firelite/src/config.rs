use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub addr: SocketAddr,
}
