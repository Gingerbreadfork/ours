//! The server the host opens on their phone.

use crate::auth::{generate_token, is_valid_token};
use crate::error::AdminError;
use crate::qr::qr_lines_for;
use crate::routes::router;
use crate::source::AdminSource;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Clone)]
pub struct AdminConfig {
    pub bind: SocketAddr,
    /// A fixed token, or `None` to generate a fresh one at startup.
    pub token: Option<String>,
}

impl fmt::Debug for AdminConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminConfig")
            .field("bind", &self.bind)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

pub struct AdminServer {
    bind: SocketAddr,
    token: String,
}

impl fmt::Debug for AdminServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminServer")
            .field("bind", &self.bind)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl AdminServer {
    /// Fails if a given token is empty, too long or has characters that
    /// would need escaping in a URL or cookie.
    pub fn new(config: AdminConfig) -> Result<Self, AdminError> {
        let token = match config.token {
            Some(token) if is_valid_token(&token) => token,
            Some(_) => return Err(AdminError::InvalidToken),
            None => generate_token(),
        };
        Ok(Self {
            bind: config.bind,
            token,
        })
    }

    pub fn bind(&self) -> SocketAddr {
        self.bind
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// `http://<bind>/?token=<token>`. With a wildcard bind such as
    /// 0.0.0.0 the host part is not reachable; use `url_with_host`.
    pub fn url(&self) -> String {
        self.url_for(self.bind)
    }

    /// The same link with the host replaced, for example by the LAN address.
    pub fn url_with_host(&self, host: IpAddr) -> String {
        self.url_for(SocketAddr::new(host, self.bind.port()))
    }

    /// `url()` as a QR code in Unicode half-blocks for the terminal.
    pub fn qr_lines(&self) -> Vec<String> {
        qr_lines_for(&self.url())
    }

    /// Binds `bind` and serves until the task is cancelled.
    pub async fn serve(self, source: Arc<dyn AdminSource>) -> Result<(), AdminError> {
        let listener = TcpListener::bind(self.bind)
            .await
            .map_err(|source| AdminError::Bind {
                addr: self.bind,
                source,
            })?;
        self.serve_listener(listener, source).await
    }

    /// Serves on a listener the caller already bound.
    pub async fn serve_listener(
        self,
        listener: TcpListener,
        source: Arc<dyn AdminSource>,
    ) -> Result<(), AdminError> {
        if let Ok(addr) = listener.local_addr() {
            tracing::info!(%addr, "admin server listening");
        }
        axum::serve(listener, router(source, self.token))
            .await
            .map_err(AdminError::Serve)
    }

    fn url_for(&self, addr: SocketAddr) -> String {
        format!("http://{addr}/?token={}", self.token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(token: Option<&str>) -> AdminConfig {
        AdminConfig {
            bind: SocketAddr::from(([127, 0, 0, 1], 7878)),
            token: token.map(str::to_owned),
        }
    }

    #[test]
    fn a_missing_token_is_generated() -> anyhow::Result<()> {
        let server = AdminServer::new(config(None))?;
        assert_eq!(server.token().len(), 43);
        assert_eq!(
            server.url(),
            format!("http://127.0.0.1:7878/?token={}", server.token())
        );
        Ok(())
    }

    #[test]
    fn a_given_token_is_kept_and_a_bad_one_refused() -> anyhow::Result<()> {
        let server = AdminServer::new(config(Some("fixed-token")))?;
        assert_eq!(server.url(), "http://127.0.0.1:7878/?token=fixed-token");
        assert!(matches!(
            AdminServer::new(config(Some("not ok"))),
            Err(AdminError::InvalidToken)
        ));
        Ok(())
    }

    #[test]
    fn the_host_can_be_replaced() -> anyhow::Result<()> {
        let server = AdminServer::new(config(Some("t")))?;
        let lan = IpAddr::from([192, 168, 1, 20]);
        assert_eq!(
            server.url_with_host(lan),
            "http://192.168.1.20:7878/?token=t"
        );
        Ok(())
    }

    #[test]
    fn the_qr_code_has_many_lines() -> anyhow::Result<()> {
        let server = AdminServer::new(config(None))?;
        assert!(server.qr_lines().len() > 10);
        Ok(())
    }

    #[test]
    fn debug_hides_the_token() -> anyhow::Result<()> {
        let server = AdminServer::new(config(Some("sekret")))?;
        assert!(!format!("{server:?}").contains("sekret"));
        assert!(!format!("{:?}", config(Some("sekret"))).contains("sekret"));
        Ok(())
    }
}
