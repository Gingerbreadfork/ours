use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ours_types::GroupId;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use serde::Deserialize;
use tracing::{Instrument, Span, field, info, info_span, warn};

use crate::wire::{self, Reply};
use crate::{ChatRequest, ChatResponse, ModelError, Pricing, SpendMeter};

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const DEFAULT_MAX_RETRIES: u32 = 3;
const DEFAULT_BACKOFF_BASE: Duration = Duration::from_millis(500);
const DEFAULT_BACKOFF_MAX: Duration = Duration::from_secs(8);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
/// The most tokens a cut-off answer is asked for again with.
const GROWN_TOKENS_MAX: u32 = 4000;

const MAX_ERROR_BODY: usize = 4096;

/// A large model behind some transport. The engine sees only this.
#[async_trait]
pub trait LargeModel: Send + Sync {
    /// Asks for one JSON value matching the request's schema, charged to
    /// `group`. The caller validates the value.
    async fn complete(
        &self,
        group: &GroupId,
        request: ChatRequest,
    ) -> Result<ChatResponse, ModelError>;

    /// A short name for logs and the pinned message.
    fn name(&self) -> &str;
}

/// How the schema is sent, and so which servers the client suits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    /// `response_format` with a strict `json_schema`, posted to
    /// `{base_url}/chat/completions`. OpenAI and the servers that copy it,
    /// such as vLLM, SGLang and the hosted routers.
    #[default]
    OpenAi,
    /// The schema in the top-level `json_schema` field, posted to
    /// `{base_url}/chat/completions`. The llama.cpp server.
    LlamaCpp,
    /// The schema in `format`, posted to `{base_url}/api/chat`. The native
    /// Ollama API, so `base_url` is the server root.
    Ollama,
}

/// What a host sets to point the client at an endpoint.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    pub model: String,
    #[serde(default)]
    pub dialect: Dialect,
    /// Seconds each attempt may take.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Extra attempts after the first.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Leave unset for a local model; a call then costs nothing.
    #[serde(default)]
    pub pricing: Option<Pricing>,
    /// Extra top-level fields merged into every request body, for server
    /// options the dialect does not know, such as `chat_template_kwargs`.
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

fn default_max_retries() -> u32 {
    DEFAULT_MAX_RETRIES
}

impl ModelConfig {
    /// A config with the OpenAI dialect, no key, no pricing and the
    /// default timeout and retry count.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            model: model.into(),
            dialect: Dialect::default(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_retries: DEFAULT_MAX_RETRIES,
            pricing: None,
            extra: serde_json::Map::new(),
        }
    }
}

impl std::fmt::Debug for ModelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("model", &self.model)
            .field("dialect", &self.dialect)
            .field("timeout_secs", &self.timeout_secs)
            .field("max_retries", &self.max_retries)
            .field("pricing", &self.pricing)
            .field("extra", &self.extra)
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
struct Backoff {
    base: Duration,
    max: Duration,
}

/// A large model on an OpenAI-style HTTP endpoint, local or cloud.
#[derive(Debug)]
pub struct OpenAiCompatible {
    client: reqwest::Client,
    config: ModelConfig,
    url: String,
    timeout: Duration,
    backoff: Backoff,
    meter: Arc<SpendMeter>,
}

/// One failed attempt and whether another may follow.
struct AttemptError {
    error: ModelError,
    retryable: bool,
    retry_after: Option<Duration>,
}

impl OpenAiCompatible {
    /// Builds the client. Calls are charged to `meter`, which the host
    /// keeps a handle to for the admin page.
    pub fn new(config: ModelConfig, meter: Arc<SpendMeter>) -> Result<Self, ModelError> {
        let base = reqwest::Url::parse(&config.base_url)
            .map_err(|e| ModelError::Config(format!("base_url: {e}")))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(ModelError::Config("base_url must be http or https".into()));
        }
        if config.model.trim().is_empty() {
            return Err(ModelError::Config("model must not be empty".into()));
        }
        if config.timeout_secs == 0 {
            return Err(ModelError::Config("timeout_secs must be at least 1".into()));
        }
        if config.pricing.is_some_and(|p| !p.is_valid()) {
            return Err(ModelError::Config(
                "pricing rates must be finite and not negative".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .user_agent(concat!("ours-model/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ModelError::Config(format!("http client: {e}")))?;
        let url = format!(
            "{}/{}",
            config.base_url.trim_end_matches('/'),
            wire::path(config.dialect)
        );
        Ok(Self {
            client,
            url,
            timeout: Duration::from_secs(config.timeout_secs),
            backoff: Backoff {
                base: DEFAULT_BACKOFF_BASE,
                max: DEFAULT_BACKOFF_MAX,
            },
            config,
            meter,
        })
    }

    /// Sets the first retry delay and the longest delay between attempts.
    /// Each delay doubles up to `max`, with jitter.
    pub fn with_backoff(mut self, base: Duration, max: Duration) -> Self {
        self.backoff = Backoff { base, max };
        self
    }

    /// The spend meter this client charges.
    pub fn meter(&self) -> &Arc<SpendMeter> {
        &self.meter
    }

    async fn run(
        &self,
        group: &GroupId,
        request: &ChatRequest,
        span: &Span,
    ) -> Result<ChatResponse, ModelError> {
        if !request.temperature.is_finite() {
            return Err(ModelError::Config("temperature must be finite".into()));
        }
        self.meter.check(group)?;

        let started = Instant::now();
        let mut request = request.clone();
        let mut body = wire::request_body(
            self.config.dialect,
            &self.config.model,
            &request,
            &self.config.extra,
        )?;
        let attempts = self.config.max_retries.saturating_add(1);
        let mut attempt = 0u32;
        let mut grown = false;
        loop {
            attempt += 1;
            span.record("attempt", attempt);
            match self.send_once(&body).await {
                Ok(text) => match self.finish(group, &text, started, span) {
                    // A reasoning model can spend the whole budget before
                    // the answer; one more go with room, then give up.
                    Err(ModelError::MalformedOutput { .. })
                        if !grown && wire::was_truncated(self.config.dialect, &text) =>
                    {
                        grown = true;
                        request.max_tokens =
                            request.max_tokens.saturating_mul(3).min(GROWN_TOKENS_MAX);
                        body = wire::request_body(
                            self.config.dialect,
                            &self.config.model,
                            &request,
                            &self.config.extra,
                        )?;
                        warn!(
                            max_tokens = request.max_tokens,
                            "answer was cut off by the token limit; asking again with more room"
                        );
                    }
                    other => return other,
                },
                Err(failure) => {
                    if !failure.retryable || attempt >= attempts {
                        return Err(failure.error);
                    }
                    let delay = self.delay(attempt, failure.retry_after);
                    warn!(
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        error = %failure.error,
                        "model call failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    async fn send_once(&self, body: &[u8]) -> Result<String, AttemptError> {
        let mut request = self
            .client
            .post(&self.url)
            .timeout(self.timeout)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_vec());
        if let Some(key) = &self.config.api_key {
            request = request.bearer_auth(key);
        }

        let response = request.send().await.map_err(transport_failure)?;
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let text = response.text().await.map_err(transport_failure)?;
        if status.is_success() {
            return Ok(text);
        }
        Err(AttemptError {
            error: ModelError::Http {
                status: status.as_u16(),
                body: truncate(text, MAX_ERROR_BODY),
            },
            retryable: status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
            retry_after,
        })
    }

    fn finish(
        &self,
        group: &GroupId,
        text: &str,
        started: Instant,
        span: &Span,
    ) -> Result<ChatResponse, ModelError> {
        let malformed = || ModelError::MalformedOutput {
            raw: text.to_owned(),
        };
        tracing::trace!(raw = %text, "model reply");
        let Reply {
            content,
            usage,
            model,
        } = wire::parse_reply(self.config.dialect, text).map_err(|_| malformed())?;

        let cost = self.config.pricing.map_or(0.0, |p| p.cost(&usage));
        self.meter.record(group, cost);
        let latency_ms = started.elapsed().as_millis() as u64;
        span.record("latency_ms", latency_ms);
        span.record("input_tokens", usage.input_tokens);
        span.record("output_tokens", usage.output_tokens);
        span.record("cached_input_tokens", usage.cached_input_tokens);
        span.record("cost_usd", cost);

        let content = content.ok_or_else(malformed)?;
        let json =
            wire::parse_json(&content).ok_or(ModelError::MalformedOutput { raw: content })?;
        Ok(ChatResponse {
            json,
            usage,
            model: model.unwrap_or_else(|| self.config.model.clone()),
            latency_ms,
        })
    }

    /// The wait before the next attempt. A server's Retry-After wins over
    /// the computed backoff.
    fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(wait) = retry_after {
            return wait.min(MAX_RETRY_AFTER);
        }
        let doublings = attempt.saturating_sub(1).min(16);
        let ceiling = self
            .backoff
            .base
            .saturating_mul(1 << doublings)
            .min(self.backoff.max);
        let half = ceiling / 2;
        half + half.mul_f64(unit_random())
    }
}

#[async_trait]
impl LargeModel for OpenAiCompatible {
    async fn complete(
        &self,
        group: &GroupId,
        request: ChatRequest,
    ) -> Result<ChatResponse, ModelError> {
        let span = info_span!(
            "model.complete",
            model = %self.config.model,
            group = %group,
            attempt = field::Empty,
            latency_ms = field::Empty,
            input_tokens = field::Empty,
            output_tokens = field::Empty,
            cached_input_tokens = field::Empty,
            cost_usd = field::Empty,
        );
        let result = self
            .run(group, &request, &span)
            .instrument(span.clone())
            .await;
        span.in_scope(|| match &result {
            Ok(_) => info!("model call finished"),
            Err(error) => warn!(%error, "model call failed"),
        });
        result
    }

    fn name(&self) -> &str {
        &self.config.model
    }
}

fn transport_failure(error: reqwest::Error) -> AttemptError {
    let error = error.without_url();
    if error.is_timeout() {
        return AttemptError {
            error: ModelError::Timeout,
            retryable: true,
            retry_after: None,
        };
    }
    let retryable = !error.is_builder();
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    AttemptError {
        error: ModelError::Transport(message),
        retryable,
        retry_after: None,
    }
}

/// Retry-After as a number of seconds. The HTTP-date form is not read.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds: f64 = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds.min(3600.0)))
}

fn truncate(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// A random number in [0, 1), good enough to spread out retries.
fn unit_random() -> f64 {
    let bits = RandomState::new().build_hasher().finish() >> 11;
    bits as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(base: Duration, max: Duration) -> OpenAiCompatible {
        OpenAiCompatible::new(
            ModelConfig::new("http://localhost:1", "m"),
            Arc::new(SpendMeter::default()),
        )
        .unwrap()
        .with_backoff(base, max)
    }

    #[test]
    fn backoff_doubles_within_jitter_and_is_capped() {
        let c = client(Duration::from_millis(100), Duration::from_millis(500));
        for _ in 0..50 {
            let first = c.delay(1, None);
            assert!(
                (50..=100).contains(&(first.as_millis() as u64)),
                "{first:?}"
            );
            let third = c.delay(3, None);
            assert!(
                (200..=400).contains(&(third.as_millis() as u64)),
                "{third:?}"
            );
            let capped = c.delay(10, None);
            assert!(
                (250..=500).contains(&(capped.as_millis() as u64)),
                "{capped:?}"
            );
        }
    }

    #[test]
    fn retry_after_wins_and_is_bounded() {
        let c = client(Duration::from_millis(100), Duration::from_millis(500));
        assert_eq!(
            c.delay(1, Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
        assert_eq!(c.delay(1, Some(Duration::from_secs(900))), MAX_RETRY_AFTER);
    }

    #[test]
    fn retry_after_reads_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(3)));
        headers.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
        headers.insert(RETRY_AFTER, "-1".parse().unwrap());
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("abc".into(), 10), "abc");
        assert_eq!(truncate("a\u{e9}b".into(), 2), "a");
    }

    #[test]
    fn config_deserialises_with_defaults() {
        let config: ModelConfig = serde_json::from_str(
            r#"{"base_url": "http://localhost:8080/v1", "model": "qwen", "dialect": "llama_cpp"}"#,
        )
        .unwrap();
        assert_eq!(config.dialect, Dialect::LlamaCpp);
        assert_eq!(config.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert_eq!(config.max_retries, DEFAULT_MAX_RETRIES);
        assert!(config.api_key.is_none() && config.pricing.is_none());
    }

    #[test]
    fn config_rejects_unknown_fields() {
        let parsed: Result<ModelConfig, _> =
            serde_json::from_str(r#"{"base_url": "http://x", "model": "m", "timeout": 5}"#);
        assert!(parsed.is_err());
    }

    #[test]
    fn debug_hides_the_api_key() {
        let mut config = ModelConfig::new("http://x", "m");
        config.api_key = Some("sk-secret".into());
        assert!(!format!("{config:?}").contains("sk-secret"));
    }
}
