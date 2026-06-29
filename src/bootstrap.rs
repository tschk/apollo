//! Runtime/bootstrap helpers used by the CLI entrypoint.

use std::path::Path;
use std::sync::Arc;

use crate::config::Config;
use crate::memory::embeddings::{create_embedding_provider, EmbeddingProvider};
use crate::memory::search::{MemoryGetTool, MemorySearchTool, SessionSearchTool};
use crate::memory::surreal::SurrealMemory;
use crate::memory::MemoryBackend;
use crate::policy::ExecutionPolicy;
#[cfg(feature = "provider-anthropic")]
use crate::providers::anthropic::AnthropicProvider;
#[cfg(feature = "provider-ollama")]
use crate::providers::ollama::OllamaProvider;
use crate::providers::openai_compat::OpenAiCompatProvider;
use crate::providers::Provider;
use crate::tools::embeddings::{EmbeddingSearchTool, EmbeddingStatusTool, EmbeddingStoreTool};
use crate::tools::file_ops::{FileReadTool, FileWriteTool};
use crate::tools::shell::ShellTool;
use crate::tools::skill_manager::SkillManagerTool;
use crate::tools::toolsets::is_tool_enabled;
use crate::tools::{
    BriefTool, ConfigTool, SleepTool, TodoWriteTool, Tool, VibemaniaTool, WorktreeTool,
};

pub fn load_config(path: &str) -> Config {
    load_config_workspace(path, None)
}

pub fn load_config_workspace(path: &str, workspace: Option<&Path>) -> Config {
    let mut cfg = Config::load(path).unwrap_or_else(|_| {
        tracing::warn!("Config not found at {}, using defaults", path);
        Config::default_config()
    });
    if let Some(ws) = workspace {
        crate::plugins::apply_workspace_manifest(&mut cfg, ws);
    }

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        cfg.provider.api_key = Some(key.clone());
        if key.contains("sk-ant-oat") && cfg.model.is_empty() {
            cfg.model = "claude-sonnet-4-5".to_string();
        }
    }

    if cfg.provider.api_key.is_none() {
        if let Ok(token) = resolve_openclaw_token("anthropic") {
            cfg.provider.api_key = Some(token);
            if cfg.model.is_empty() {
                cfg.model = "claude-sonnet-4-5".to_string();
            }
        }
        #[cfg(feature = "provider-anthropic")]
        {
            if let Ok(_provider) =
                crate::providers::anthropic::AnthropicProvider::from_env_or_oauth()
            {
                let _ = _provider;
                if let Ok((token, _, _)) = crate::providers::oauth::load_oauth_token_from_file() {
                    cfg.provider.api_key = Some(token);
                    cfg.model = "claude-sonnet-4-5".to_string();
                }
            }
        }

        if cfg.provider.api_key.is_none() {
            if let Ok(key) = std::env::var("OPENAI_API_KEY") {
                cfg.provider.name = "openai".to_string();
                cfg.provider.api_key = Some(key);
            }
        }
    }

    if cfg.provider.name == "ollama" && cfg.provider.base_url.is_none() {
        if let Ok(url) = std::env::var("OLLAMA_BASE_URL") {
            cfg.provider.base_url = Some(url);
        }
    }

    if cfg.embeddings.api_key.is_none() {
        match cfg.embeddings.provider.as_str() {
            "openai" | "openai_compat" => {
                if let Ok(key) = std::env::var("OPENAI_API_KEY") {
                    cfg.embeddings.api_key = Some(key);
                }
            }
            "ollama" | "local" => {
                if cfg.embeddings.base_url.is_none() {
                    if let Ok(url) = std::env::var("OLLAMA_BASE_URL") {
                        cfg.embeddings.base_url = Some(url);
                    }
                }
            }
            "gemini" => {
                if let Ok(key) = std::env::var("GEMINI_API_KEY") {
                    cfg.embeddings.api_key = Some(key);
                }
            }
            _ => {}
        }
    }

    cfg
}

pub fn build_provider(cfg: &Config) -> Arc<dyn Provider> {
    let api_key = cfg.provider.api_key.clone().unwrap_or_default();

    match cfg.provider.name.as_str() {
        #[cfg(feature = "provider-anthropic")]
        "anthropic" | "claude" => {
            let mut p = AnthropicProvider::new(&api_key);
            if let Some(url) = &cfg.provider.base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        #[cfg(feature = "provider-copilot")]
        "github-copilot" | "copilot" => {
            if let Ok(p) = crate::providers::copilot::CopilotProvider::from_openclaw() {
                Arc::new(p)
            } else {
                Arc::new(crate::providers::copilot::CopilotProvider::new(&api_key))
            }
        }
        "ollama" => {
            #[cfg(feature = "provider-ollama")]
            {
                let url = cfg
                    .provider
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "http://localhost:11434".into());
                Arc::new(OllamaProvider::new(url))
            }
            #[cfg(not(feature = "provider-ollama"))]
            {
                panic!("provider=ollama requires building with the provider-ollama feature");
            }
        }
        "openai" => Arc::new(OpenAiCompatProvider::openai(&api_key)),
        "openrouter" => Arc::new(OpenAiCompatProvider::openrouter(&api_key)),
        "groq" => Arc::new(OpenAiCompatProvider::groq(&api_key)),
        "together" => Arc::new(OpenAiCompatProvider::together(&api_key)),
        "mistral" => Arc::new(OpenAiCompatProvider::mistral(&api_key)),
        "deepseek" => Arc::new(OpenAiCompatProvider::deepseek(&api_key)),
        "fireworks" => Arc::new(OpenAiCompatProvider::fireworks(&api_key)),
        "perplexity" => Arc::new(OpenAiCompatProvider::perplexity(&api_key)),
        "xai" | "grok" => Arc::new(OpenAiCompatProvider::xai(&api_key)),
        "moonshot" | "kimi" => Arc::new(OpenAiCompatProvider::moonshot(&api_key)),
        "venice" => Arc::new(OpenAiCompatProvider::venice(&api_key)),
        "huggingface" => Arc::new(OpenAiCompatProvider::huggingface(&api_key)),
        "siliconflow" => Arc::new(OpenAiCompatProvider::siliconflow(&api_key)),
        "cerebras" => Arc::new(OpenAiCompatProvider::cerebras(&api_key)),
        "minimax" => Arc::new(OpenAiCompatProvider::minimax(&api_key)),
        "vercel" => Arc::new(OpenAiCompatProvider::vercel(&api_key)),
        other => {
            let url = cfg
                .provider
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".into());
            Arc::new(OpenAiCompatProvider::new(&api_key, url, other))
        }
    }
}

pub fn build_base_tools(
    workspace: &Path,
    policy: Arc<ExecutionPolicy>,
    memory: Arc<dyn MemoryBackend>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    provider: Arc<dyn Provider>,
    cfg: &Config,
) -> Vec<Arc<dyn Tool>> {
    let toolsets = &cfg.toolsets;
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ShellTool::new(workspace.to_path_buf(), Arc::clone(&policy))),
        Arc::new(FileReadTool::new(workspace.to_path_buf())),
        Arc::new(FileWriteTool::new(workspace.to_path_buf())),
        Arc::new(crate::tools::edit::EditTool::new(workspace.to_path_buf())),
        Arc::new(MemorySearchTool::new(workspace.to_path_buf())),
        Arc::new(MemoryGetTool::new(workspace.to_path_buf())),
        Arc::new(SessionSearchTool::new(Arc::clone(&memory))),
        Arc::new(crate::tools::web_search::WebSearchTool::new()),
        Arc::new(crate::tools::web_fetch::WebFetchTool::new()),
        Arc::new(crate::tools::doctor::DoctorTool::new()),
        Arc::new(crate::tools::session::ListModelsTool::new()),
        Arc::new(crate::tools::dynamic::CreateToolTool::new(Arc::clone(
            &policy,
        ))),
        Arc::new(crate::tools::dynamic::ListCustomToolsTool::new()),
        Arc::new(crate::tools::browser::BrowserTool::new()),
        Arc::new(crate::tools::mcp::McpTool::new()),
        Arc::new(SkillManagerTool::new(workspace.to_path_buf())),
        Arc::new(BriefTool::new(
            Arc::clone(&provider),
            cfg.agent.fast_model.clone(),
        )),
        Arc::new(ConfigTool::new(workspace.join("unthinkclaw.json"))),
        Arc::new(SleepTool),
        Arc::new(TodoWriteTool::new(workspace.to_path_buf())),
        Arc::new(VibemaniaTool::new(workspace.to_path_buf())),
        Arc::new(WorktreeTool::new(workspace.to_path_buf())),
    ];
    if let Some(provider) = embedding_provider {
        tools.push(Arc::new(EmbeddingStatusTool::new(Arc::clone(&provider))));
        tools.push(Arc::new(EmbeddingStoreTool::new(
            Arc::clone(&provider),
            Arc::clone(&memory),
        )));
        tools.push(Arc::new(EmbeddingSearchTool::new(provider, memory)));
    }
    #[cfg(feature = "peekaboo")]
    tools.push(Arc::new(crate::tools::PeekabooTool));
    #[cfg(feature = "rs-gbrain")]
    if cfg.rs_gbrain.enabled {
        tools.push(Arc::new(crate::tools::rs_gbrain::BrainSearchTool));
        tools.push(Arc::new(crate::tools::rs_gbrain::BrainQueryTool));
        tools.push(Arc::new(crate::tools::rs_gbrain::BrainPutTool));
        tools.push(Arc::new(crate::tools::rs_gbrain::BrainGetTool));
    }
    tools
        .into_iter()
        .filter(|tool| is_tool_enabled(tool.name(), toolsets))
        .collect()
}

pub fn build_embedding_provider(
    cfg: &Config,
) -> anyhow::Result<Option<Arc<dyn EmbeddingProvider>>> {
    if !cfg.embeddings.enabled {
        return Ok(None);
    }

    let provider_name = cfg.embeddings.provider.trim().to_ascii_lowercase();
    let model = cfg.embeddings.model.clone();
    let base_url = cfg.embeddings.base_url.clone();
    let api_key = cfg.embeddings.api_key.clone();

    let provider = create_embedding_provider(&provider_name, api_key, model, base_url)?;
    Ok(Some(provider))
}

pub async fn build_memory_backend(
    workspace: &Path,
    cfg: &Config,
) -> anyhow::Result<Arc<dyn MemoryBackend>> {
    let storage_root = workspace.join(&cfg.storage.root);
    std::fs::create_dir_all(&storage_root)?;
    let backend = cfg.storage.backend.trim().to_ascii_lowercase();
    if backend != "surreal" {
        anyhow::bail!(
            "storage.backend={} is not supported; only surreal is available",
            cfg.storage.backend
        );
    }

    let surreal_path = storage_root.join("memory.surreal");
    let memory = SurrealMemory::new(surreal_path.as_path()).await?;
    Ok(Arc::new(memory))
}

fn resolve_openclaw_token(provider: &str) -> anyhow::Result<String> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("No home dir"))?;
    let auth_path = home.join(".openclaw/agents/main/agent/auth-profiles.json");

    if !auth_path.exists() {
        return Err(anyhow::anyhow!("No auth-profiles.json found"));
    }

    let content = std::fs::read_to_string(&auth_path)?;
    let data: serde_json::Value = serde_json::from_str(&content)?;

    let profile_key = format!("{}:default", provider);
    if let Some(profile) = data["profiles"][&profile_key].as_object() {
        if let Some(token) = profile.get("token").and_then(|t| t.as_str()) {
            if !token.is_empty() {
                tracing::info!("Loaded {} token from OpenClaw auth-profiles", provider);
                return Ok(token.to_string());
            }
        }
        if let Some(key) = profile.get("key").and_then(|k| k.as_str()) {
            if !key.is_empty() {
                tracing::info!("Loaded {} API key from OpenClaw auth-profiles", provider);
                return Ok(key.to_string());
            }
        }
    }

    if let Some(profiles) = data["profiles"].as_object() {
        for (key, value) in profiles {
            if let Some(p) = value["provider"].as_str() {
                if p == provider {
                    if let Some(token) = value["token"].as_str() {
                        if !token.is_empty() {
                            tracing::info!(
                                "Loaded {} token from OpenClaw profile {}",
                                provider,
                                key
                            );
                            return Ok(token.to_string());
                        }
                    }
                    if let Some(key_val) = value["key"].as_str() {
                        if !key_val.is_empty() {
                            tracing::info!("Loaded {} key from OpenClaw profile {}", provider, key);
                            return Ok(key_val.to_string());
                        }
                    }
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "No {} credentials in auth-profiles",
        provider
    ))
}
