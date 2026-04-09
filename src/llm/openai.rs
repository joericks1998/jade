use super::{InferenceBackend, InferenceRequest, InferenceResponse};
use crate::interpreter::error::{JadeError, Result, Span};

pub struct OpenAiBackend {
    api_key: String,
    default_model: String,
    client: reqwest::blocking::Client,
}

impl OpenAiBackend {
    pub fn new(api_key: &str, default_model: &str) -> Self {
        OpenAiBackend {
            api_key: api_key.to_string(),
            default_model: default_model.to_string(),
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl InferenceBackend for OpenAiBackend {
    fn infer(&self, req: InferenceRequest, span: Span) -> Result<InferenceResponse> {
        let model = if req.model.is_empty() { &self.default_model } else { &req.model };

        // Build messages array from history + new user prompt
        let mut messages: Vec<serde_json::Value> = req.history.iter().map(|m| {
            serde_json::json!({ "role": m.role, "content": m.content })
        }).collect();
        messages.push(serde_json::json!({ "role": "user", "content": req.prompt }));

        let body = serde_json::json!({
            "model": model,
            "messages": messages,
        });

        let response = self.client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| JadeError::InferenceError { message: e.to_string(), span })?;

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().unwrap_or_default();
            return Err(JadeError::InferenceError {
                message: format!("OpenAI API returned HTTP {}: {}", status, body_text),
                span,
            });
        }

        let json: serde_json::Value = response.json()
            .map_err(|e| JadeError::InferenceError { message: e.to_string(), span })?;

        let text = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| JadeError::InferenceError {
                message: "unexpected response format from OpenAI API".to_string(),
                span,
            })?
            .trim()
            .to_string();

        let tokens_used = json["usage"]["total_tokens"].as_u64().unwrap_or(0) as i64;

        Ok(InferenceResponse { text, tokens_used })
    }
}
