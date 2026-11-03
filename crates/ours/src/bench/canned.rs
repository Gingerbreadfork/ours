//! A model that gives the same answer to everything, for runs with no model.

use async_trait::async_trait;
use ours_model::{ChatRequest, ChatResponse, LargeModel, ModelError, Usage};
use ours_types::GroupId;
use serde_json::json;

/// The name a canned model reports.
pub const CANNED: &str = "canned";

/// Answers every request with one fixed plan: `reply` and no actions.
#[derive(Debug, Clone)]
pub struct CannedModel {
    reply: String,
}

impl CannedModel {
    pub fn new(reply: impl Into<String>) -> CannedModel {
        CannedModel {
            reply: reply.into(),
        }
    }

    /// The canned "ok" the rules-only runs use.
    pub fn ok() -> CannedModel {
        CannedModel::new("ok")
    }
}

#[async_trait]
impl LargeModel for CannedModel {
    async fn complete(
        &self,
        _group: &GroupId,
        _request: ChatRequest,
    ) -> Result<ChatResponse, ModelError> {
        Ok(ChatResponse {
            json: json!({ "message": self.reply, "actions": [], "cites": [] }),
            usage: Usage::default(),
            model: CANNED.to_owned(),
            latency_ms: 0,
        })
    }

    fn name(&self) -> &str {
        CANNED
    }
}
