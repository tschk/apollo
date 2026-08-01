use async_trait::async_trait;
use rs_ai_oauth::codex::{codex_request_body, ChatGptCodexClient};
use serde_json::{json, Value};

use super::traits::{
    ChatMessage, ChatRequest, ChatResponse, Provider, ProviderCapabilities, ToolCall,
};

pub struct CodexProvider {
    client: ChatGptCodexClient,
}

impl CodexProvider {
    pub fn new(access_token: impl Into<String>) -> Self {
        Self {
            client: ChatGptCodexClient::new(access_token).with_originator("apollo"),
        }
    }
}

#[async_trait]
impl Provider for CodexProvider {
    fn name(&self) -> &str {
        "chatgpt"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tools: true,
            streaming: false,
            vision: true,
            max_context: 272_000,
            native_web_search: false,
        }
    }

    async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        let input = messages_to_responses_input(request.messages);
        let tools = request
            .tools
            .unwrap_or(&[])
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                    "strict": null,
                })
            })
            .collect();
        let instructions = request
            .messages
            .iter()
            .find(|message| message.role == "system")
            .map(|message| message.content.as_str())
            .unwrap_or("You are a helpful assistant.");
        let body = codex_request_body(request.model, instructions, input, tools, None);
        let response = self.client.complete(body, None).await?;
        Ok(ChatResponse {
            text: (!response.text.is_empty()).then_some(response.text),
            tool_calls: response
                .tool_calls
                .into_iter()
                .map(|call| ToolCall {
                    id: call.id,
                    name: call.name,
                    arguments: call.arguments,
                })
                .collect(),
            usage: Some(super::traits::Usage {
                input_tokens: response.input_tokens as u32,
                output_tokens: response.output_tokens as u32,
            }),
        })
    }
}

fn messages_to_responses_input(messages: &[ChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|message| match message.role.as_str() {
            "system" => None,
            "tool_result" => Some(json!({
                "type": "function_call_output",
                "call_id": message.tool_use_id.clone().unwrap_or_default(),
                "output": message.content,
            })),
            "assistant" => Some(json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": message.content, "annotations": []}],
                "status": "completed",
            })),
            _ => Some(json!({
                "role": "user",
                "content": [{"type": "input_text", "text": message.content}],
            })),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_messages_to_responses_input() {
        let messages = [ChatMessage::user("hello")];
        let input = messages_to_responses_input(&messages);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
    }
}
