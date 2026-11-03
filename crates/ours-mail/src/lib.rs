//! ours-mail: the group's email address. SMTP out through lettre, IMAP in
//! through async-imap, and file-based stand-ins for hosts without either
//! (DESIGN.md sections 13 and 18). Mail that arrives is always a guest.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod config;
mod error;
mod parse;
mod poll;
mod recv;
mod send;

pub use config::{ImapConfig, MailConfig, SmtpConfig};
pub use error::MailError;
pub use parse::parse_eml;
pub use poll::poll_loop;
pub use recv::{DirInbox, ImapInbox, Inbox};
pub use send::{FileMailer, Mailer, SmtpMailer, generate_message_id};
