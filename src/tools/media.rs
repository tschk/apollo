//! Media generation tools backed by `rs_ai` — image, text-to-speech and
//! speech-to-text.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine as _;
use serde::Deserialize;

use crate::redaction;
use crate::tools::{Tool, ToolResult, ToolSpec};

fn resolve_api_key(
    provider: &str,
    arg_key: Option<String>,
    default: &str,
) -> anyhow::Result<String> {
    if let Some(k) = arg_key {
        if !k.is_empty() {
            return Ok(k);
        }
    }
    let env_var = match provider {
        "chatgpt" | "openai" => "OPENAI_API_KEY",
        "gemini" => "GOOGLE_API_KEY",
        "xai" | "grok" => "XAI_API_KEY",
        "claude" => "ANTHROPIC_API_KEY",
        _ => "OPENAI_API_KEY",
    };
    if let Ok(k) = std::env::var(env_var) {
        if !k.is_empty() {
            return Ok(k);
        }
    }
    if !default.is_empty() {
        return Ok(default.to_string());
    }
    anyhow::bail!("no API key available for provider {provider}")
}

fn default_image_model(provider: &str) -> &str {
    match provider {
        "chatgpt" | "openai" => "dall-e-3",
        "gemini" => "imagen-3.0-generate-002",
        "xai" | "grok" => "grok-2a-image",
        _ => "dall-e-3",
    }
}

fn default_tts_model(provider: &str) -> &str {
    match provider {
        "chatgpt" | "openai" => "tts-1",
        "gemini" => "gemini-2.5-flash",
        "xai" | "grok" => "grok-4.20-realtime",
        _ => "tts-1",
    }
}

fn default_stt_model(provider: &str) -> &str {
    match provider {
        "chatgpt" | "openai" => "whisper-1",
        "gemini" => "gemini-2.5-flash",
        "xai" | "grok" => "grok-4.20-realtime",
        _ => "whisper-1",
    }
}

fn normalize_provider(p: &str) -> &str {
    match p {
        "openai" => "chatgpt",
        "grok" => "xai",
        other => other,
    }
}

fn build_client(
    provider: &str,
    api_key: &str,
    model: &str,
) -> anyhow::Result<rs_ai::ClientBuilder> {
    let builder = match normalize_provider(provider) {
        "chatgpt" => rs_ai::chatgpt(),
        "gemini" => rs_ai::gemini(),
        "xai" => rs_ai::xai(),
        "claude" => rs_ai::claude(),
        _ => rs_ai::chatgpt(),
    };
    Ok(builder.api_key(api_key).model(model))
}

async fn save_generated_file(
    workspace: &Path,
    bytes: Vec<u8>,
    media_type: &str,
) -> anyhow::Result<String> {
    let ext = match media_type {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "audio/mp3" | "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/webm" => "webm",
        _ => "bin",
    };
    let dir = workspace.join(".apollo").join("generated");
    tokio::fs::create_dir_all(&dir).await?;
    let name = format!("{}.{}", uuid::Uuid::new_v4(), ext);
    let path = dir.join(name);
    tokio::fs::write(&path, bytes).await?;
    Ok(path.to_string_lossy().to_string())
}

/// Generate an image from a text prompt.
pub struct ImageGenerationTool {
    workspace: PathBuf,
    default_api_key: String,
    default_provider: String,
}

impl ImageGenerationTool {
    pub fn new(workspace: PathBuf, default_api_key: String, default_provider: String) -> Self {
        Self {
            workspace,
            default_api_key,
            default_provider,
        }
    }
}

#[derive(Deserialize)]
struct GenerateImageArgs {
    prompt: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    n: Option<u32>,
    #[serde(default)]
    aspect_ratio: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

#[async_trait]
impl Tool for ImageGenerationTool {
    fn name(&self) -> &str {
        "generate_image"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "generate_image".to_string(),
            description: "Generate an image from a text prompt using ChatGPT DALL-E, Gemini Imagen, or xAI Grok Imagine.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["prompt"],
                "properties": {
                    "prompt": { "type": "string", "description": "image description" },
                    "provider": { "type": "string", "enum": ["chatgpt", "gemini", "xai"], "description": "provider to use" },
                    "model": { "type": "string", "description": "specific model id" },
                    "size": { "type": "string", "description": "size like 1024x1024" },
                    "n": { "type": "integer", "description": "number of images" },
                    "aspect_ratio": { "type": "string", "description": "aspect ratio like 16:9" },
                    "api_key": { "type": "string", "description": "optional api key override" }
                }
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: GenerateImageArgs = serde_json::from_str(arguments)?;
        let provider = args.provider.as_deref().unwrap_or(&self.default_provider);
        let model = args
            .model
            .as_deref()
            .unwrap_or(default_image_model(provider));
        let api_key = resolve_api_key(provider, args.api_key, &self.default_api_key)?;

        let mut options = rs_ai_core::ImageGenerationOptions::default();
        if let Some(n) = args.n {
            options.n = Some(n);
        }
        if let Some(size) = args.size {
            options.size = Some(size);
        }
        if let Some(ar) = args.aspect_ratio {
            options.aspect_ratio = Some(ar);
        }

        let result = build_client(provider, &api_key, model)?
            .generate_image(&args.prompt, options)
            .await
            .map_err(|e| anyhow::anyhow!("image generation failed: {e}"))?;

        let file = result.image;
        let path = save_generated_file(&self.workspace, file.bytes, &file.media_type).await?;
        let output = serde_json::json!({
            "file_path": redaction::redact_text(&path),
            "media_type": file.media_type,
            "images_generated": result.images.len().max(1),
        });
        Ok(ToolResult::success(output.to_string()))
    }
}

/// Synthesize speech from text.
pub struct TextToSpeechTool {
    workspace: PathBuf,
    default_api_key: String,
    default_provider: String,
}

impl TextToSpeechTool {
    pub fn new(workspace: PathBuf, default_api_key: String, default_provider: String) -> Self {
        Self {
            workspace,
            default_api_key,
            default_provider,
        }
    }
}

#[derive(Deserialize)]
struct TtsArgs {
    text: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

#[async_trait]
impl Tool for TextToSpeechTool {
    fn name(&self) -> &str {
        "text_to_speech"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "text_to_speech".to_string(),
            description: "Synthesize speech from text and return an audio file path.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": { "type": "string" },
                    "provider": { "type": "string", "enum": ["chatgpt", "gemini", "xai"], "description": "provider to use" },
                    "model": { "type": "string", "description": "model id like tts-1" },
                    "api_key": { "type": "string" }
                }
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: TtsArgs = serde_json::from_str(arguments)?;
        let provider = args.provider.as_deref().unwrap_or(&self.default_provider);
        let model = args.model.as_deref().unwrap_or(default_tts_model(provider));
        let api_key = resolve_api_key(provider, args.api_key, &self.default_api_key)?;

        let bytes = build_client(provider, &api_key, model)?
            .speak(&args.text)
            .await
            .map_err(|e| anyhow::anyhow!("text-to-speech failed: {e}"))?;

        let path = save_generated_file(&self.workspace, bytes, "audio/mp3").await?;
        let output = serde_json::json!({"file_path": redaction::redact_text(&path)});
        Ok(ToolResult::success(output.to_string()))
    }
}

/// Transcribe audio to text.
pub struct SpeechToTextTool {
    default_api_key: String,
    default_provider: String,
}

impl SpeechToTextTool {
    pub fn new(default_api_key: String, default_provider: String) -> Self {
        Self {
            default_api_key,
            default_provider,
        }
    }
}

#[derive(Deserialize)]
struct SttArgs {
    #[serde(default)]
    audio_path: Option<String>,
    #[serde(default)]
    audio_base64: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

#[async_trait]
impl Tool for SpeechToTextTool {
    fn name(&self) -> &str {
        "speech_to_text"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "speech_to_text".to_string(),
            description: "Transcribe audio from a file path or base64 string to text.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "audio_path": { "type": "string", "description": "path to audio file" },
                    "audio_base64": { "type": "string", "description": "base64-encoded audio bytes" },
                    "mime_type": { "type": "string", "description": "audio mime type like audio/mp3" },
                    "provider": { "type": "string", "enum": ["chatgpt", "gemini", "xai"], "description": "provider to use" },
                    "model": { "type": "string", "description": "model id like whisper-1" },
                    "api_key": { "type": "string" }
                }
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: SttArgs = serde_json::from_str(arguments)?;
        let provider = args.provider.as_deref().unwrap_or(&self.default_provider);
        let model = args.model.as_deref().unwrap_or(default_stt_model(provider));
        let api_key = resolve_api_key(provider, args.api_key, &self.default_api_key)?;

        let audio = if let Some(path) = args.audio_path {
            tokio::fs::read(&path).await?
        } else if let Some(b64) = args.audio_base64 {
            base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .map_err(|e| anyhow::anyhow!("invalid base64 audio: {e}"))?
        } else {
            anyhow::bail!("provide either audio_path or audio_base64")
        };

        let mime_type = args.mime_type.unwrap_or_else(|| "audio/mp3".to_string());
        let text = build_client(provider, &api_key, model)?
            .transcribe(audio, &mime_type)
            .await
            .map_err(|e| anyhow::anyhow!("speech-to-text failed: {e}"))?;

        Ok(ToolResult::success(text))
    }
}
