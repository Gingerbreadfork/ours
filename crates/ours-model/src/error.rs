use ours_types::GroupId;
use thiserror::Error;

/// Everything a model call can fail with.
#[derive(Debug, Error)]
pub enum ModelError {
    /// The endpoint answered with a non-success status.
    #[error("endpoint returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// The endpoint could not be reached or the connection broke.
    #[error("could not reach the endpoint: {0}")]
    Transport(String),
    /// An attempt ran past the configured timeout.
    #[error("the endpoint did not answer in time")]
    Timeout,
    /// The reply arrived but did not hold the JSON the schema asked for.
    #[error("model output was not valid JSON")]
    MalformedOutput { raw: String },
    /// The group has reached its daily spend cap; no request was made.
    #[error("group {group} has spent ${spent:.2} of its ${cap:.2} daily cap")]
    CapExceeded {
        group: GroupId,
        spent: f64,
        cap: f64,
    },
    /// The client was configured with something unusable.
    #[error("invalid model configuration: {0}")]
    Config(String),
}
