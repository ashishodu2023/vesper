use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, Write};

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
    pub num_predict: i32,
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

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    #[serde(rename = "type")]
    pub type_: &'static str,
    pub function: ToolFunctionSpec,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolFunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone)]
pub struct NativeToolCall {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct ChatOutcome {
    pub content: String,
    pub tool_calls: Vec<NativeToolCall>,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Preferred chat options for this client (temperature / ctx / predict).
    fn chat_options(&self) -> ChatOptions {
        ChatOptions::default()
    }

    async fn chat(&self, messages: &[ChatMessage]) -> Result<String>;

    async fn chat_with_options(
        &self,
        messages: &[ChatMessage],
        _opts: &ChatOptions,
    ) -> Result<String> {
        self.chat(messages).await
    }

    /// Stream tokens via channel for live UX. Falls back to a single send if needed.
    async fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        opts: &ChatOptions,
        on_token: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<String> {
        let reply = self.chat_with_options(messages, opts).await?;
        if !reply.is_empty() {
            let _ = on_token.send(reply.clone());
        }
        Ok(reply)
    }

    /// Optional native tool-calling. Default: unsupported.
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolSpec],
        opts: &ChatOptions,
    ) -> Result<ChatOutcome> {
        let content = self.chat_with_options(messages, opts).await?;
        Ok(ChatOutcome {
            content,
            tool_calls: vec![],
        })
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
                .timeout(std::time::Duration::from_secs(300))
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

    pub fn options(&self) -> &ChatOptions {
        &self.options
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
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolSpec]>,
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
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<OllamaToolCall>,
}

#[derive(Deserialize)]
struct OllamaToolCall {
    function: OllamaToolFunction,
}

#[derive(Deserialize)]
struct OllamaToolFunction {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Deserialize)]
struct OllamaStreamChunk {
    #[serde(default)]
    message: Option<OllamaStreamMessage>,
    #[serde(default)]
    done: bool,
}

#[derive(Deserialize)]
struct OllamaStreamMessage {
    #[serde(default)]
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

fn to_messages<'a>(messages: &'a [ChatMessage]) -> Vec<OllamaMessage<'a>> {
    messages
        .iter()
        .map(|m| OllamaMessage {
            role: &m.role,
            content: &m.content,
        })
        .collect()
}

#[async_trait]
impl LlmClient for OllamaClient {
    fn chat_options(&self) -> ChatOptions {
        self.options.clone()
    }

    async fn chat(&self, messages: &[ChatMessage]) -> Result<String> {
        self.chat_with_options(messages, &self.options).await
    }

    async fn chat_with_options(
        &self,
        messages: &[ChatMessage],
        opts: &ChatOptions,
    ) -> Result<String> {
        let outcome = self.chat_with_tools(messages, &[], opts).await?;
        Ok(outcome.content)
    }

    async fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        opts: &ChatOptions,
        on_token: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<String> {
        let payload = OllamaChatRequest {
            model: &self.model,
            messages: to_messages(messages),
            stream: true,
            keep_alive: &opts.keep_alive,
            options: OllamaOptions {
                temperature: opts.temperature,
                num_ctx: opts.num_ctx,
                num_predict: opts.num_predict,
            },
            tools: None,
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
            return Err(anyhow!("Ollama stream failed ({status}): {body}"));
        }

        let mut stream = response.bytes_stream();
        let mut buf = String::new();
        let mut full = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("read stream chunk")?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf = buf[pos + 1..].to_string();
                if line.is_empty() {
                    continue;
                }
                let parsed: OllamaStreamChunk = serde_json::from_str(&line)
                    .with_context(|| format!("bad stream JSON: {line}"))?;
                if let Some(msg) = parsed.message {
                    if !msg.content.is_empty() {
                        let _ = on_token.send(msg.content.clone());
                        full.push_str(&msg.content);
                    }
                }
                if parsed.done {
                    let _ = io::stderr().flush();
                    return Ok(full);
                }
            }
        }
        Ok(full)
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        opts: &ChatOptions,
    ) -> Result<ChatOutcome> {
        let payload = OllamaChatRequest {
            model: &self.model,
            messages: to_messages(messages),
            stream: false,
            keep_alive: &opts.keep_alive,
            options: OllamaOptions {
                temperature: opts.temperature,
                num_ctx: opts.num_ctx,
                num_predict: opts.num_predict,
            },
            tools: if tools.is_empty() { None } else { Some(tools) },
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

        let tool_calls = parsed
            .message
            .tool_calls
            .into_iter()
            .map(|t| {
                let arguments = match t.function.arguments {
                    Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
                    other => other,
                };
                NativeToolCall {
                    name: t.function.name,
                    arguments,
                }
            })
            .collect();

        Ok(ChatOutcome {
            content: parsed.message.content,
            tool_calls,
        })
    }
}

/// Print streaming tokens to stderr without buffering newlines oddly.
pub fn eprint_token(token: &str) {
    eprint!("{token}");
    let _ = io::stderr().flush();
}
