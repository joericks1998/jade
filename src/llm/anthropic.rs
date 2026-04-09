use super::{InferenceBackend, InferenceRequest, InferenceResponse};
use crate::interpreter::error::{JadeError, Result, Span};

pub struct AnthropicBackend {
    api_key: String,
    default_model: String,
    client: reqwest::blocking::Client,
}

impl AnthropicBackend {
    pub fn new(api_key: &str, default_model: &str) -> Self {
        AnthropicBackend {
            api_key: api_key.to_string(),
            default_model: default_model.to_string(),
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl InferenceBackend for AnthropicBackend {
    fn infer(&self, req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
        let model = if req.model.is_empty() { &self.default_model } else { &req.model };

        // Build messages array from history + new user prompt
        let mut messages: Vec<serde_json::Value> = req.history.iter().map(|m| {
            serde_json::json!({ "role": m.role, "content": m.content })
        }).collect();
        messages.push(serde_json::json!({ "role": "user", "content": req.prompt }));

        let body = serde_json::json!({
            "model": model,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });

        let response = self.client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| JadeError::InferenceError { message: e.to_string(), span })?;

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().unwrap_or_default();
            return Err(JadeError::InferenceError {
                message: format!("Anthropic API returned HTTP {}: {}", status, body_text),
                span,
            });
        }

        let json: serde_json::Value = response.json()
            .map_err(|e| JadeError::InferenceError { message: e.to_string(), span })?;

        let text = json["content"][0]["text"]
            .as_str()
            .ok_or_else(|| JadeError::InferenceError {
                message: "unexpected response format from Anthropic API".to_string(),
                span,
            })?
            .trim()
            .to_string();

        let tokens_used = (json["usage"]["input_tokens"].as_u64().unwrap_or(0)
            + json["usage"]["output_tokens"].as_u64().unwrap_or(0)) as i64;

        Ok(InferenceResponse { text, tokens_used })
    }
}
