//! Quick LLM smoke test — builds the real `MiniMaxClient` from
//! `config/llm.yaml` and sends one ping. Use to verify end-to-end that
//! your Token Plan / API key + base URL + api_flavor picked by the
//! wizard actually work against MiniMax.
//!
//! ```
//! cargo run --bin llm_smoke
//! cargo run --bin llm_smoke -- "hola como estas"   # custom prompt
//! ```

use std::path::PathBuf;

use agent_config::AppConfig;
use agent_llm::{ChatMessage, ChatRequest, ChatRole, LlmRegistry, ResponseContent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,agent_llm=debug")),
        )
        .init();

    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Responde sólo con la palabra PONG.".into());

    let config_dir =
        PathBuf::from(std::env::var("AGENT_CONFIG_DIR").unwrap_or_else(|_| "./config".into()));
    println!("▸ loading config from {}", config_dir.display());
    let cfg = AppConfig::load(&config_dir)?;

    // Pick the first agent + its model — same resolution the runtime uses.
    let agent = cfg
        .agents
        .agents
        .first()
        .ok_or_else(|| anyhow::anyhow!("no agents in agents.yaml"))?;
    println!("▸ agent        : {}", agent.id);
    println!(
        "▸ model        : {}/{}",
        agent.model.provider, agent.model.model
    );

    if let Some(provider) = cfg.llm.providers.get(&agent.model.provider) {
        println!("▸ base_url     : {}", provider.base_url);
        println!(
            "▸ api_flavor   : {}",
            provider.api_flavor.as_deref().unwrap_or("(default)")
        );
        println!(
            "▸ auth.mode    : {}",
            provider
                .auth
                .as_ref()
                .map(|a| a.mode.as_str())
                .unwrap_or("(none)")
        );
    }

    let registry = LlmRegistry::with_builtins();
    let client = registry.build(&cfg.llm, &agent.model)?;

    println!();
    println!("▸ sending prompt: {prompt:?}");
    let req = ChatRequest {
        model: agent.model.model.clone(),
        system_prompt: Some("Eres un asistente que responde muy corto.".into()),
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: prompt,
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }],
        tools: vec![],
        max_tokens: 128,
        temperature: 0.2,
        stop_sequences: Vec::new(),
        tool_choice: Default::default(),
    
        system_blocks: Vec::new(),
        cache_tools: false,
    };

    let start = std::time::Instant::now();
    let resp = client.chat(req).await?;
    let elapsed = start.elapsed();

    println!();
    println!("──── Response ────");
    match resp.content {
        ResponseContent::Text(t) => println!("{t}"),
        ResponseContent::ToolCalls(calls) => {
            println!("(tool_calls)");
            for c in calls {
                println!("  {} {}", c.name, c.arguments);
            }
        }
    }
    println!("──────────────────");
    println!(
        "usage: {} + {} tokens · finish: {:?} · took {:?}",
        resp.usage.prompt_tokens, resp.usage.completion_tokens, resp.finish_reason, elapsed
    );
    println!();
    println!("✔ smoke passed");
    Ok(())
}
