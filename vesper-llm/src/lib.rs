use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatOptions {
    pub temperature: f32,
    pub num_ctx: u32,
    /// Cap generated tokens (big latency win for short answers).
    pub num_predict: i32,
    /// Keep model loaded in VRAM between requests.
    pub keep_alive: String,
}

impl Default for ChatOptions {
    fn default() -> Self {
        Self {
            temperature: 0.2,
            num_ctx: 4096,
            num_predict: 768,
            keep_alive: "30m".into(),
        }
    }
}

impl ChatOptions {
    pub fn fast() -> Self {
        Self {
            temperature: 0.2,
            num_ctx: 3072,
            num_predict: 384,
            keep_alive: "30m".into(),
        }
    }

    pub fn agent() -> Self {
        Self {
            temperature: 0.2,
            num_ctx: 4096,
            num_predict: 640,
            keep_alive: "30m".into(),
        }
    }
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, messages: &[ChatMessage]) -> Result<String>;
    async fn chat_with_options(
        &self,
        messages: &[ChatMessage],
        _opts: &ChatOptions,
    ) -> Result<String> {
        self.chat(messages).await
    }
}

#[derive(Debug, Clone)]
pub struct OllamaClient {
    base_url: String,
    model: String,
    http: reqwest::Client,
    options: ChatOptions,
}

impl OllamaClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(180))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            options: ChatOptions::default(),
        }
    }

    pub fn with_options(mut self, options: ChatOptions) -> Self {
        self.options = options;
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/tags", self.base_url);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("failed to reach Ollama at {url}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("Ollama tags failed ({status}): {body}"));
        }
        let parsed: TagsResponse = response.json().await?;
        let mut names: Vec<String> = parsed.models.into_iter().map(|m| m.name).collect();
        names.sort();
        Ok(names)
    }
}

#[derive(Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: Vec<OllamaMessage<'a>>,
    stream: bool,
    keep_alive: &'a str,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaOptions {
    temperature: f32,
    num_ctx: u32,
    num_predict: i32,
}

#[derive(Serialize)]
struct OllamaMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaResponseMessage,
}

#[derive(Deserialize)]
struct OllamaResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
}

#[async_trait]
impl LlmClient for OllamaClient {
    async fn chat(&self, messages: &[ChatMessage]) -> Result<String> {
        self.chat_with_options(messages, &self.options).await
    }

    async fn chat_with_options(
        &self,
        messages: &[ChatMessage],
        opts: &ChatOptions,
    ) -> Result<String> {
        let payload = OllamaChatRequest {
            model: &self.model,
            messages: messages
                .iter()
                .map(|m| OllamaMessage {
                    role: &m.role,
                    content: &m.content,
                })
                .collect(),
            stream: false,
            keep_alive: &opts.keep_alive,
            options: OllamaOptions {
                temperature: opts.temperature,
                num_ctx: opts.num_ctx,
                num_predict: opts.num_predict,
            },
        };

        let url = format!("{}/api/chat", self.base_url);
        let response = self
            .http
            .post(&url)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("failed to reach Ollama at {url}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("Ollama chat failed ({status}): {body}"));
        }

        let parsed: OllamaChatResponse = response
            .json()
            .await
            .context("invalid JSON from Ollama /api/chat")?;
        Ok(parsed.message.content)
    }
}
