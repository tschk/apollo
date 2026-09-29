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
    note: "the login stays in the rs_ai credential store on the machine running apollo. this phone records the choice, not the token.",
  },
  {
    id: "github-copilot",
    label: "GitHub Copilot",
    auth: "oauth",
    defaultModel: "gpt-5.4",
    blurb: "sign in with github",
    note: "apollo uses this only when the binary is built with provider-copilot. no token is stored here.",
  },
  {
    id: "claude",
    label: "Claude",
    auth: "oauth",
    defaultModel: "claude-sonnet-5",
    blurb: "not a sign-in on this phone",
    note: "apollo routes anthropic and claude configs to chatgpt today. the card is here so the choice matches the desktop.",
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
    note: "the key is held in memory for this session and written, on the desktop, to the instance .env.",
    baseUrl: "https://openrouter.ai/api/v1",
  },
  {
    id: "openai",
    label: "OpenAI",
    auth: "api-key",
    envVar: "OPENAI_API_KEY",
    defaultModel: "gpt-5.4",
    blurb: "api key",
    note: "sent only as a bearer token to the provider. it is not put in desktop.json.",
    baseUrl: "https://api.openai.com/v1",
  },
  {
    id: "gemini",
    label: "Gemini",
    auth: "api-key",
    envVar: "GEMINI_API_KEY",
    defaultModel: "gemini-3.1-pro-preview",
    blurb: "google ai",
    note: "uses the openai-compatible gemini endpoint.",
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
  },
  {
    id: "xai",
    label: "xAI",
    auth: "api-key",
    envVar: "XAI_API_KEY",
    defaultModel: "grok-build-0.1",
    blurb: "api key",
    note: "sent only as a bearer token to the provider.",
    baseUrl: "https://api.x.ai/v1",
  },
  {
    id: "deepseek",
    label: "DeepSeek",
    auth: "api-key",
    envVar: "DEEPSEEK_API_KEY",
    defaultModel: "deepseek-v4-pro",
    blurb: "api key",
    note: "sent only as a bearer token to the provider.",
    baseUrl: "https://api.deepseek.com",
  },
  {
    id: "moonshot",
    label: "Moonshot",
    auth: "api-key",
    envVar: "MOONSHOT_API_KEY",
    defaultModel: "kimi-k3",
    blurb: "api key",
    note: "sent only as a bearer token to the provider.",
    baseUrl: "https://api.moonshot.ai/v1",
  },
  {
    id: "ollama",
    label: "Ollama",
    auth: "local",
    defaultModel: "llama3.2",
    blurb: "no key",
    note: "talks to ollama on this device at 127.0.0.1. a model server somewhere else is a custom endpoint you type.",
    baseUrl: "http://127.0.0.1:11434/v1",
  },
  {
    id: "custom",
    label: "Custom endpoint",
    auth: "custom",
    envVar: "APOLLO_PROVIDER_API_KEY",
    defaultModel: "",
    blurb: "any openai-compatible api",
    note: "base url, optional key, model. the key variable matches the desktop: APOLLO_PROVIDER_API_KEY.",
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
  { id: "auto", label: "auto", detail: "default heuristics, shell enabled — recommended" },
  { id: "prompt", label: "prompt", detail: "approve plans before tools run" },
  { id: "tools_only", label: "tools only", detail: "web, memory and sessions — no shell, no file writes" },
  { id: "full", label: "full", detail: "autonomous on this workspace — shell and file writes" },
];

export function profileById(id: string): ProfileInfo {
  return PROFILES.find((p) => p.id === id) ?? PROFILES[0];
}
