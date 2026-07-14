use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use vesper_agent::{Agent, AgentOptions, SessionMode};
use vesper_llm::{ChatMessage, LlmClient};
use vesper_tools::Workspace;

#[derive(Clone)]
struct ScriptLlm {
    replies: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl LlmClient for ScriptLlm {
    async fn chat(&self, _messages: &[ChatMessage]) -> anyhow::Result<String> {
        let mut q = self.replies.lock().unwrap();
        if q.is_empty() {
            anyhow::bail!("no more scripted replies");
        }
        Ok(q.remove(0))
    }
}

#[tokio::test]
async fn tool_loop_lists_and_finishes() {
    let dir = std::env::temp_dir().join(format!(
        "vesper-loop-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hello.txt"), "hi from vesper").unwrap();

    let llm = ScriptLlm {
        replies: Arc::new(Mutex::new(vec![
            r#"{"action":"tool","name":"list_dir","args":{"path":"."}}"#.into(),
            r#"{"action":"tool","name":"read_file","args":{"path":"hello.txt"}}"#.into(),
            r#"{"action":"final","message":"Found hello.txt with hi from vesper"}"#.into(),
        ])),
    };
    let ws = Workspace::new(&dir).unwrap();
    let mut agent = Agent::new(llm, ws);
    let result = agent
        .run(
            "inspect the workspace",
            AgentOptions::for_mode(SessionMode::Auto, 8, None, Box::new(|_, _| true)),
            |_| {},
        )
        .await
        .unwrap();

    assert!(!result.truncated);
    assert_eq!(result.steps, 2);
    assert!(result.message.contains("hello.txt"));
    let _ = std::fs::remove_dir_all(dir);
}
