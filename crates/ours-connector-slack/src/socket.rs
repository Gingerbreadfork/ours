//! The Socket Mode connection: ask for a URL with the app-level token, read
//! frames, acknowledge each envelope at once, and reconnect with backoff when
//! the connection drops. Events go to the connector's loop in order.

use crate::api::{Api, ApiError};
use crate::event::{self, Event, Frame};
use futures::{SinkExt as _, StreamExt as _};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant as Clock;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
};

/// How often the connection is pinged.
const PING_EVERY: Duration = Duration::from_secs(30);

/// How long without any frame before the connection is taken as dead.
const SILENCE: Duration = Duration::from_secs(90);

/// The longest wait between reconnection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Event ids remembered to drop Slack's redeliveries.
const REMEMBERED: usize = 2048;

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// What the socket task tells the connector's loop.
#[derive(Debug)]
pub enum SocketMessage {
    /// A connection is up. `resync` is set when events may have been lost
    /// while it was down.
    Connected {
        resync: bool,
    },
    Event(Box<Event>),
    /// The connection cannot be made again without the host's help.
    Fatal(String),
}

/// The wait before reconnection attempt `failures`, doubling from a second.
pub fn backoff(failures: u32) -> Duration {
    Duration::from_secs(1u64 << failures.min(6)).min(MAX_BACKOFF)
}

/// Why the app-level token cannot open Socket Mode, or `None` when the
/// failure is worth retrying.
pub fn app_token_failure(error: &ApiError) -> Option<String> {
    if error.rejects_token() || error.code == "missing_scope" {
        Some(format!(
            "the Slack app-level token was rejected ({}): check app_token (xapp-) in the config; \
             it needs the connections:write scope",
            error.code
        ))
    } else {
        None
    }
}

/// Why the bot token cannot be used, or `None` when the failure is worth retrying.
pub fn bot_token_failure(error: &ApiError) -> Option<String> {
    error.rejects_token().then(|| {
        format!(
            "the Slack bot token was rejected ({}): check token (xoxb-) in the config, \
             and that the app is still installed",
            error.code
        )
    })
}

/// A sentence for the host when Slack says not to reconnect.
pub fn disconnect_failure(reason: &str) -> Option<String> {
    (reason == "link_disabled").then(|| {
        "Slack turned Socket Mode off for this app; switch it on under Socket Mode in the app settings"
            .to_owned()
    })
}

/// Event ids already passed on. Slack sends an event again when it missed
/// the acknowledgement, often on the next connection.
#[derive(Debug, Default)]
pub struct Seen {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    /// True the first time an id is offered.
    pub fn first(&mut self, id: &str) -> bool {
        if !self.ids.insert(id.to_owned()) {
            return false;
        }
        self.order.push_back(id.to_owned());
        if self.order.len() > REMEMBERED
            && let Some(oldest) = self.order.pop_front()
        {
            self.ids.remove(&oldest);
        }
        true
    }
}

enum Ended {
    /// Slack asked for a fresh connection.
    Refresh,
    Lost,
    Fatal(String),
    /// The connector stopped listening.
    Closed,
}

/// Keep a Socket Mode connection up until the connector stops listening or
/// the app-level token is refused.
pub async fn run(
    api: Arc<Api>,
    tls: Arc<rustls::ClientConfig>,
    tx: mpsc::UnboundedSender<SocketMessage>,
) {
    let mut failures = 0u32;
    let mut resync = false;
    let mut seen = Seen::default();
    loop {
        if tx.is_closed() {
            return;
        }
        let url = match api.socket_url().await {
            Ok(url) => url,
            Err(error) => {
                if let Some(reason) = app_token_failure(&error) {
                    let _ = tx.send(SocketMessage::Fatal(reason));
                    return;
                }
                tracing::warn!(%error, "could not open a Socket Mode connection");
                resync = true;
                tokio::time::sleep(backoff(failures)).await;
                failures = failures.saturating_add(1);
                continue;
            }
        };
        let connector = Some(Connector::Rustls(Arc::clone(&tls)));
        let socket = match connect_async_tls_with_config(url.as_str(), None, false, connector).await
        {
            Ok((socket, _)) => socket,
            Err(error) => {
                tracing::warn!(%error, "Socket Mode websocket did not connect");
                resync = true;
                tokio::time::sleep(backoff(failures)).await;
                failures = failures.saturating_add(1);
                continue;
            }
        };
        match session(socket, &tx, &mut resync, &mut failures, &mut seen).await {
            Ended::Refresh => tracing::debug!("Socket Mode refresh"),
            Ended::Lost => {
                tracing::warn!("Socket Mode connection lost; reconnecting");
                resync = true;
                tokio::time::sleep(backoff(failures)).await;
                failures = failures.saturating_add(1);
            }
            Ended::Fatal(reason) => {
                let _ = tx.send(SocketMessage::Fatal(reason));
                return;
            }
            Ended::Closed => return,
        }
    }
}

/// Read one connection until it ends.
async fn session(
    mut socket: Socket,
    tx: &mpsc::UnboundedSender<SocketMessage>,
    resync: &mut bool,
    failures: &mut u32,
    seen: &mut Seen,
) -> Ended {
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut heard = Clock::now();
    loop {
        tokio::select! {
            frame = socket.next() => {
                heard = Clock::now();
                let text = match frame {
                    Some(Ok(WsMessage::Text(text))) => text,
                    Some(Ok(WsMessage::Close(frame))) => {
                        tracing::debug!(?frame, "Socket Mode closed");
                        return Ended::Lost;
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Socket Mode read failed");
                        return Ended::Lost;
                    }
                    None => return Ended::Lost,
                };
                match event::parse_frame(text.as_str()) {
                    Frame::Hello => {
                        *failures = 0;
                        let resync = std::mem::take(resync);
                        if tx.send(SocketMessage::Connected { resync }).is_err() {
                            return Ended::Closed;
                        }
                    }
                    Frame::Disconnect(reason) => {
                        let _ = socket.close(None).await;
                        return match disconnect_failure(&reason) {
                            Some(failure) => Ended::Fatal(failure),
                            None => Ended::Refresh,
                        };
                    }
                    Frame::Event { envelope, id, event } => {
                        if socket.send(WsMessage::text(event::ack(&envelope))).await.is_err() {
                            return Ended::Lost;
                        }
                        if !seen.first(&id) {
                            tracing::debug!(event = %id, "dropped a redelivered event");
                        } else if tx.send(SocketMessage::Event(event)).is_err() {
                            return Ended::Closed;
                        }
                    }
                    Frame::Other { envelope: Some(envelope) } => {
                        if socket.send(WsMessage::text(event::ack(&envelope))).await.is_err() {
                            return Ended::Lost;
                        }
                    }
                    Frame::Other { envelope: None } => tracing::debug!("skipped a Socket Mode frame"),
                }
            }
            _ = ping.tick() => {
                if heard.elapsed() > SILENCE {
                    tracing::warn!("Socket Mode went quiet");
                    return Ended::Lost;
                }
                if socket.send(WsMessage::Ping(Default::default())).await.is_err() {
                    return Ended::Lost;
                }
                if tx.is_closed() {
                    return Ended::Closed;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redelivered_event_is_passed_on_once() {
        let mut seen = Seen::default();
        assert!(seen.first("Ev1"));
        assert!(!seen.first("Ev1"));
        assert!(seen.first("Ev2"));
        for n in 0..REMEMBERED {
            seen.first(&format!("filler-{n}"));
        }
        assert!(seen.first("Ev1"), "old ids are forgotten");
        assert_eq!(seen.order.len(), REMEMBERED);
    }

    #[test]
    fn backoff_doubles_to_a_minute() {
        let waits: Vec<u64> = (0..8).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(backoff(u32::MAX), MAX_BACKOFF);
    }

    #[test]
    fn a_refused_token_is_fatal_and_names_the_token() {
        let app =
            app_token_failure(&ApiError::new("apps.connections.open", "invalid_auth")).unwrap();
        assert!(app.contains("app_token (xapp-)") && app.contains("connections:write"));
        assert!(
            app_token_failure(&ApiError::new("apps.connections.open", "missing_scope")).is_some()
        );
        assert!(
            app_token_failure(&ApiError::new("apps.connections.open", "ratelimited")).is_none()
        );
        assert!(
            app_token_failure(&ApiError::new(
                "apps.connections.open",
                "HTTP 503 Service Unavailable"
            ))
            .is_none()
        );

        let bot = bot_token_failure(&ApiError::new("auth.test", "token_revoked")).unwrap();
        assert!(bot.contains("token (xoxb-)") && bot.contains("token_revoked"));
        assert!(bot_token_failure(&ApiError::new("auth.test", "fatal_error")).is_none());
    }

    #[test]
    fn only_link_disabled_stops_reconnection() {
        assert!(
            disconnect_failure("link_disabled")
                .unwrap()
                .contains("Socket Mode")
        );
        assert_eq!(disconnect_failure("refresh_requested"), None);
        assert_eq!(disconnect_failure("warning"), None);
    }
}
