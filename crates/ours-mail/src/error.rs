//! The one error type of the crate.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MailError {
    /// The configuration cannot be used: a bad address, a missing section.
    #[error("mail config: {0}")]
    Config(String),
    /// The mail could not be delivered or the file could not be written;
    /// trying again later may work.
    #[error("mail transport: {0}")]
    Transport(String),
    /// The server refused the mail for good. `code` is the SMTP reply code.
    #[error("mail rejected ({code}): {message}")]
    Rejected { code: u16, message: String },
    /// A message could not be read as mail.
    #[error("mail parse: {0}")]
    Parse(String),
    /// The IMAP server or connection failed.
    #[error("imap: {0}")]
    Imap(String),
}

impl From<std::io::Error> for MailError {
    fn from(e: std::io::Error) -> Self {
        MailError::Transport(e.to_string())
    }
}
