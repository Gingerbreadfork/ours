//! Errors the admin server can return to the binary.

use std::net::SocketAddr;

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("the admin token must be 1 to 128 characters from A-Z a-z 0-9 - _ . ~")]
    InvalidToken,
    #[error("cannot bind the admin server to {addr}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("the admin server stopped")]
    Serve(#[source] std::io::Error),
}
