use crate::channels::IncomingMessage;

const GROUP_KEYWORDS: &[&str] = &[
    "apollo",
    "plugin",
    "plugins",
    "plugin layer",
    "manifest",
    "settings",
    "command",
    "commands",
    "upgrade",
    "upgrades",
    "transport",
    "help",
    "bot",
    "module",
    "modules",
    "configure",
    "configuration",
    "how do i",
    "what is",
    "what does",
    "why is",
    "can you",
    "could you",
];

const GENERAL_ASSISTANT_PATTERNS: &[&str] = &[
    "what can you do",
    "how does this work",
    "how do i",
    "what is this",
    "what does this do",
    "can you help",
    "could you help",
    "setup",
    "set up",
    "configure",
    "configuration",
    "plugin",
    "plugins",
    "manifest",
    "layer",
    "transport",
    "command",
    "commands",
];

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

pub fn is_assistant_topic(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }

    contains_any(&lower, GROUP_KEYWORDS) || contains_any(&lower, GENERAL_ASSISTANT_PATTERNS)
}

pub fn should_respond(msg: &IncomingMessage) -> bool {
    if !msg.is_group {
        return true;
    }

    let text = msg.text.trim().to_lowercase();
    if text.is_empty() {
        return false;
    }

    if text.starts_with('/') || text.starts_with('!') {
        return true;
    }

    let direct_mention = text.contains("@apollo");
    direct_mention || is_assistant_topic(&text)
}

pub fn routing_guidance(is_group: bool, transport: &str) -> Option<String> {
    if !is_group {
        return None;
    }

    Some(format!(
        "## Group chat routing
Transport: {transport}

Respond even without a direct mention when the message is about apollo, apollo-live, plugins, plugin manifests, settings, commands, upgrades, transport, or asks for help with the bot. Stay silent for unrelated ambient chatter. When responding, be concise and reference the relevant command, plugin, or setting path when useful.",
        transport = transport
    ))
}

pub fn plugin_layer_policy_prompt() -> String {
    "## Plugin layer policy
Treat live functionality as a plugin layer on top of core. Prefer plugin manifests, hooks, and layered overrides rather than hardcoding live-only behavior into core runtime paths. Keep plugin-specific configuration isolated from core defaults so upgrades remain compatible.
".to_string()
}

pub fn transport_policy_prompt(transport: &str) -> String {
    format!(
        "## Transport policy
Default transport: {transport}

Treat the configured transport as a thin adapter over the core runtime, with settings overrides isolated from the core config so upgrades do not conflict. Use context-aware routing for group chats, but only answer ambient messages when they are clearly about the assistant, its plugins, or operational commands.",
        transport = transport
    )
}

pub fn group_memory_key(chat_id: &str) -> String {
    format!("group:{chat_id}")
}

pub fn group_memory_prompt(chat_id: &str, summary: &str) -> String {
    format!(
        "## Rolling group memory\nChat: {chat_id}\n\n{}",
        summary.trim()
    )
}
