//! patchify - Fixes read->patch verification loops and patch->patch bursts atomically with rollback.
pub mod update;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchRequest {
    pub items: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchResult {
    pub ok: bool,
    pub data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Batch entry point - implement real batching here.
pub async fn batch(req: BatchRequest) -> Result<Vec<BatchResult>> {
    // TODO: implement batched patchify logic
    let _ = req;
    Ok(vec![])
}
