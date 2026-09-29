/**
 * Chat from this device.
 *
 * A key-based provider is called at its own API. Ollama is called on
 * 127.0.0.1 of this device. OAuth choices do not carry a token here, so
 * the reply is an offline mock and labeled as one — the same idea as the
 * desktop probe when no credential is available.
 *
 * This module never calls the apollo agent HTTP API. That API is the
 * process on the machine where the binary runs.
 */

import type { AuthKind } from "./providers";

export interface ChatMessage {
  role: "user" | "assistant";
  text: string;
}

export interface RoutePlan {
  kind: "provider" | "offline";
  label: string;
  url?: string;
}

export function chatCompletionsUrl(base: string): string {
  const trimmed = base.trim().replace(/\/+$/, "");
  if (!trimmed) return "";
  if (trimmed.endsWith("/chat/completions")) return trimmed;
  return `${trimmed}/chat/completions`;
}

export function planRoute(input: {
  auth: AuthKind;
  baseUrl?: string;
  key?: string;
}): RoutePlan {
  if (input.auth === "oauth") {
    return {
      kind: "offline",
      label: "offline mock · sign-in stays on the machine running apollo",
    };
  }
  if (input.auth === "local") {
    const url = chatCompletionsUrl(input.baseUrl ?? "");
    if (!url) return { kind: "offline", label: "offline mock · ollama has no base url" };
    return { kind: "provider", label: "live · ollama on this device", url };
  }
  const key = input.key?.trim() ?? "";
  if (input.auth === "api-key" && !key) {
    return { kind: "offline", label: "offline mock · no key in this session" };
  }
  const url = chatCompletionsUrl(input.baseUrl ?? "");
  if (!url) {
    return { kind: "offline", label: "offline mock · custom endpoint needs a base url" };
  }
  if (input.auth === "custom" && !key) {
    return { kind: "provider", label: "live · custom endpoint, no key", url };
  }
  return { kind: "provider", label: "live · provider api", url };
}

export function scrub(text: string, secret: string): string {
  const value = secret.trim();
  if (value.length < 6) return text;
  return text.split(value).join("***");
}

export function offlineReply(label: string): string {
  return `${label}. nothing was sent.`;
}

export function parseCompletion(body: unknown): string | null {
  if (!body || typeof body !== "object") return null;
  const choices = (body as { choices?: unknown }).choices;
  if (!Array.isArray(choices) || !choices[0] || typeof choices[0] !== "object") return null;
  const message = (choices[0] as { message?: unknown }).message;
  if (!message || typeof message !== "object") return null;
  const content = (message as { content?: unknown }).content;
  return typeof content === "string" && content.trim() ? content.trim() : null;
}

export async function complete(input: {
  auth: AuthKind;
  baseUrl?: string;
  key?: string;
  model: string;
  messages: ChatMessage[];
  fetchImpl?: typeof fetch;
  timeoutMs?: number;
}): Promise<{ text: string; label: string; live: boolean }> {
  const plan = planRoute(input);
  if (plan.kind === "offline" || !plan.url) {
    return { text: offlineReply(plan.label), label: plan.label, live: false };
  }
  const key = input.key?.trim() ?? "";
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), input.timeoutMs ?? 20000);
  const headers: Record<string, string> = {
    Accept: "application/json",
    "Content-Type": "application/json",
  };
  if (key) headers.Authorization = `Bearer ${key}`;
  try {
    const response = await (input.fetchImpl ?? fetch)(plan.url, {
      method: "POST",
      headers,
      body: JSON.stringify({
        model: input.model,
        messages: input.messages.map((message) => ({
          role: message.role,
          content: message.text,
        })),
      }),
      signal: controller.signal,
    });
    const raw = await response.text();
    if (!response.ok) {
      return {
        text: `the provider returned ${response.status}. the body is not shown.`,
        label: plan.label,
        live: true,
      };
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      return { text: "the provider did not return json.", label: plan.label, live: true };
    }
    const text = parseCompletion(parsed);
    if (!text) {
      return { text: "the provider returned no message.", label: plan.label, live: true };
    }
    return { text: scrub(text, key), label: plan.label, live: true };
  } catch (error) {
    const reason = error instanceof Error ? error.message : "request failed";
    const aborted = reason.toLowerCase().includes("abort");
    return {
      text: scrub(aborted ? "the provider timed out." : "the provider could not be reached.", key),
      label: plan.label,
      live: true,
    };
  } finally {
    clearTimeout(timer);
  }
}
