//! The large-model client: one JSON object out of an OpenAI-style endpoint,
//! with retries, a timeout per attempt, cost accounting and a daily spend
//! cap per group. The client knows nothing of the engine's types; the caller
//! validates the JSON it gets back. On top of it, the classifier scores one
//! message for the triage cascade, the extractor pulls facts, commitments and ledger lines out of it and
//! the time resolver reads the time it names. DESIGN.md sections 7, 9, 12
//! and 14.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod classifier;
mod client;
mod error;
mod extractor;
mod meter;
mod pricing;
mod schema;
mod text;
mod time;
mod types;
mod wire;

pub use classifier::{Classifier, RulesOnlyClassifier, SmallModelClassifier};
pub use client::{Dialect, LargeModel, ModelConfig, OpenAiCompatible};
pub use error::ModelError;
pub use extractor::{Extractor, NoExtractor, SmallModelExtractor};
pub use meter::{DEFAULT_DAILY_CAP, SpendMeter};
pub use pricing::Pricing;
pub use time::{NoTimeResolver, SmallModelTimeResolver, TimeResolver};
pub use types::{ChatRequest, ChatResponse, ChatTurn, TurnRole, Usage};
