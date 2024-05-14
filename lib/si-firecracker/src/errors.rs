use sysctl::SysctlError;
use thiserror::Error;

#[remain::sorted]
#[derive(Error, Debug)]
pub enum FirecrackerError {
    #[error("iface error: {0}")]
    IFace(#[from] std::io::Error),
    #[error("iptables error: {0}")]
    IpTables(#[from] Box<dyn std::error::Error>),
    #[error("netns error: {0}")]
    NetNs(#[from] netns_rs::Error),
    #[error("rtnetlink error: {0}")]
    RTNetLink(#[from] rtnetlink::Error),
    #[error("sysctl error: {0}")]
    SysCtl(#[from] SysctlError),
}
