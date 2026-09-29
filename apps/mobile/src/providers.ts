/**
 * The onboarding list the desktop app pins, kept here so this package does
 * not wait on the apollo-ui branch. Ids match `provider.name` in apollo.json.
 * Key variable names match `bootstrap::catalog_env_key` on main.
 */

export type AuthKind = "oauth" | "api-key" | "local" | "custom";

export interface ProviderInfo {
  id: string;
  label: string;
  auth: AuthKind;
  /** Variable the desktop writes into the instance `.env`. Never a value. */
  envVar?: string;
  defaultModel: string;
  blurb: string;
  /** Shown on the card. Explains what this phone can and cannot do. */
  note: string;
  /** OpenAI-compatible origin, including `/v1` when the provider uses it. */
  baseUrl?: string;
}

export const OAUTH_PROVIDERS: readonly ProviderInfo[] = [
  {
    id: "chatgpt",
    label: "ChatGPT",
    auth: "oauth",
    defaultModel: "gpt-5.6",
    blurb: "sign in with your chatgpt plan",
    note: "your chatgpt plan. the phone keeps the choice, not the token.",
  },
  {
    id: "github-copilot",
    label: "GitHub Copilot",
    auth: "oauth",
    defaultModel: "gpt-5.4",
    blurb: "sign in with github",
    note: "github sign-in, when copilot is included. the phone does not keep the token.",
  },
  {
    id: "claude",
    label: "Claude",
    auth: "oauth",
    defaultModel: "claude-sonnet-5",
    blurb: "through chatgpt for now",
    note: "not a sign-in here. claude is sent through chatgpt for now.",
  },
];

export const KEY_PROVIDERS: readonly ProviderInfo[] = [
  {
    id: "openrouter",
    label: "OpenRouter",
    auth: "api-key",
    envVar: "OPENROUTER_API_KEY",
    defaultModel: "z-ai/glm-5.2",
    blurb: "one key, many models",
    note: "one key, for this session. it is not saved in the shared config.",
    baseUrl: "https://openrouter.ai/api/v1",
  },
  {
    id: "openai",
    label: "OpenAI",
    auth: "api-key",
    envVar: "OPENAI_API_KEY",
    defaultModel: "gpt-5.4",
    blurb: "api key",
    note: "for this session. not saved in the shared config.",
    baseUrl: "https://api.openai.com/v1",
  },
  {
    id: "gemini",
    label: "Gemini",
    auth: "api-key",
    envVar: "GEMINI_API_KEY",
    defaultModel: "gemini-3.1-pro-preview",
    blurb: "google ai",
    note: "a gemini key, for this session.",
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
  },
  {
    id: "xai",
    label: "xAI",
    auth: "api-key",
    envVar: "XAI_API_KEY",
    defaultModel: "grok-build-0.1",
    blurb: "api key",
    note: "for this session. not saved in the shared config.",
    baseUrl: "https://api.x.ai/v1",
  },
  {
    id: "deepseek",
    label: "DeepSeek",
    auth: "api-key",
    envVar: "DEEPSEEK_API_KEY",
    defaultModel: "deepseek-v4-pro",
    blurb: "api key",
    note: "for this session. not saved in the shared config.",
    baseUrl: "https://api.deepseek.com",
  },
  {
    id: "moonshot",
    label: "Moonshot",
    auth: "api-key",
    envVar: "MOONSHOT_API_KEY",
    defaultModel: "kimi-k3",
    blurb: "api key",
    note: "for this session. not saved in the shared config.",
    baseUrl: "https://api.moonshot.ai/v1",
  },
  {
    id: "ollama",
    label: "Ollama",
    auth: "local",
    defaultModel: "llama3.2",
    blurb: "no key",
    note: "ollama on this phone. another address is a custom endpoint.",
    baseUrl: "http://127.0.0.1:11434/v1",
  },
  {
    id: "custom",
    label: "Custom endpoint",
    auth: "custom",
    envVar: "APOLLO_PROVIDER_API_KEY",
    defaultModel: "",
    blurb: "any openai-compatible api",
    note: "an address, an optional key, and a model.",
  },
];

export const PROVIDERS: readonly ProviderInfo[] = [...OAUTH_PROVIDERS, ...KEY_PROVIDERS];

export function providerById(id: string): ProviderInfo | undefined {
  if (id === "custom" || id.startsWith("custom-")) {
    return KEY_PROVIDERS.find((p) => p.id === "custom");
  }
  return PROVIDERS.find((p) => p.id === id);
}

export interface ProfileInfo {
  id: string;
  label: string;
  detail: string;
}

/** Same ids `apollo init` writes. Enforcement happens in the agent, not here. */
export const PROFILES: readonly ProfileInfo[] = [
  { id: "auto", label: "auto", detail: "the usual choices. shell is on — recommended" },
  { id: "prompt", label: "prompt", detail: "ask before tools run" },
  { id: "tools_only", label: "tools only", detail: "web, memory and sessions — no shell, no file writes" },
  { id: "full", label: "full", detail: "shell and file writes, without asking first" },
];

export function profileById(id: string): ProfileInfo {
  return PROFILES.find((p) => p.id === id) ?? PROFILES[0];
}
