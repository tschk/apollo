import type { DesktopState } from "./config";

export type PreviewId =
  | "welcome"
  | "provider"
  | "workspace"
  | "permissions"
  | "test"
  | "mode"
  | "simple"
  | "advanced"
  | "instances";

const IDS: readonly PreviewId[] = [
  "welcome",
  "provider",
  "workspace",
  "permissions",
  "test",
  "mode",
  "simple",
  "advanced",
  "instances",
];

export function previewFromHash(hash: string): PreviewId | null {
  const id = hash.replace(/^#/, "").trim();
  return IDS.find((item) => item === id) ?? null;
}

export function sampleState(mode: DesktopState["mode"]): DesktopState {
  return {
    onboarded: true,
    mode,
    active: "research",
    instances: [
      {
        id: "apollo",
        name: "apollo",
        everywhere: false,
        workspace: "~/src/apollo",
        config_dir: "~/src/apollo",
        provider: "openai",
        model: "gpt-5.4",
        permission_profile: "auto",
        color: null,
        pinned: false,
      },
      {
        id: "research",
        name: "research",
        everywhere: true,
        workspace: "~",
        config_dir: "~/.apollo/instances/research",
        provider: "openrouter",
        model: "z-ai/glm-5.2",
        permission_profile: "prompt",
        color: null,
        pinned: true,
      },
    ],
  };
}

export const sampleMessages = [
  { role: "user" as const, text: "what is on this desk?" },
  {
    role: "assistant" as const,
    text: "not sent · no key yet. nothing was sent.",
    route: "not sent · no key yet",
  },
];

export const sampleLog = [
  { t: "14:02", line: "saved instance research" },
  { t: "14:02", line: "mode advanced" },
  { t: "14:03", line: "ready to chat" },
];
