/**
 * The desktop document `~/.apollo/desktop.json` (apollo-ui), as data.
 *
 * Field names match that file: `onboarded`, `mode`, `active`, `instances[]`
 * with `everywhere`, `workspace`, `config_dir`, `provider`, `model`,
 * `permission_profile`, `color`, `pinned`. Secrets are not fields. A phone
 * has no apollo home directory, so the same JSON is stored in app storage
 * and can be exported. `~` means "home on the machine that runs apollo"
 * when that path is not known here.
 */

import type { ProviderInfo } from "./providers";

export type Mode = "simple" | "advanced";

export interface Instance {
  id: string;
  name: string;
  everywhere: boolean;
  workspace: string;
  config_dir: string;
  provider: string;
  model: string;
  permission_profile: string;
  color: number | null;
  pinned: boolean;
}

export interface DesktopState {
  onboarded: boolean;
  mode: Mode;
  active: string | null;
  instances: Instance[];
}

export function emptyState(): DesktopState {
  return { onboarded: false, mode: "simple", active: null, instances: [] };
}

/** Filesystem-safe id. Same rules as apollo-ui `instance_id`. */
export function instanceId(name: string, taken: readonly string[]): string {
  let base = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]/g, "-");
  while (base.includes("--")) base = base.replaceAll("--", "-");
  base = base.replace(/^-+|-+$/g, "");
  if (!base) base = "apollo";
  if (!taken.includes(base)) return base;
  let n = 2;
  while (taken.includes(`${base}-${n}`)) n += 1;
  return `${base}-${n}`;
}

export function resolveScope(opts: {
  everywhere: boolean;
  folder: string;
  instanceId: string;
  home?: string;
}): { workspace: string; config_dir: string; everywhere: boolean } {
  if (!opts.everywhere) {
    const folder = opts.folder.trim();
    return { workspace: folder, config_dir: folder, everywhere: false };
  }
  const home = (opts.home ?? "").trim().replace(/\/+$/, "") || "~";
  return {
    workspace: home,
    config_dir: `${home}/.apollo/instances/${opts.instanceId}`,
    everywhere: true,
  };
}

export function providerName(provider: ProviderInfo, customName: string, taken: readonly string[]): string {
  if (provider.id !== "custom") return provider.id;
  const slug = instanceId(customName, []);
  if (!customName.trim() || slug === "apollo") return "custom";
  const name = `custom-${slug}`;
  if (!taken.includes(name)) return name;
  let n = 2;
  while (taken.includes(`${name}-${n}`)) n += 1;
  return `${name}-${n}`;
}

/** Pinned first. Stable for the rest, matching `roster_order`. */
export function rosterOrder(instances: readonly Instance[]): Instance[] {
  return instances
    .map((instance, index) => ({ instance, index }))
    .sort((a, b) => {
      if (a.instance.pinned !== b.instance.pinned) return a.instance.pinned ? -1 : 1;
      return a.index - b.index;
    })
    .map((row) => row.instance);
}

export function scopeLabel(instance: Instance): string {
  return instance.everywhere ? "everywhere" : instance.workspace;
}

const INSTANCE_KEYS = [
  "id",
  "name",
  "everywhere",
  "workspace",
  "config_dir",
  "provider",
  "model",
  "permission_profile",
  "color",
  "pinned",
] as const;

function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

function str(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function parseInstance(value: unknown): Instance | null {
  const row = asRecord(value);
  if (!row) return null;
  const id = str(row.id).trim();
  const name = str(row.name).trim();
  if (!id || !name) return null;
  const color = typeof row.color === "number" && Number.isFinite(row.color) ? row.color : null;
  return {
    id,
    name,
    everywhere: row.everywhere === true,
    workspace: str(row.workspace),
    config_dir: str(row.config_dir),
    provider: str(row.provider),
    model: str(row.model),
    permission_profile: str(row.permission_profile, "auto") || "auto",
    color,
    pinned: row.pinned === true,
  };
}

/**
 * Accepts the current document or the pre-instances shape (one workspace).
 * Unknown keys are dropped, including anything that looks like a credential.
 */
export function parseDesktopState(raw: unknown): DesktopState {
  const row = asRecord(raw);
  if (!row) return emptyState();
  const mode: Mode = row.mode === "advanced" ? "advanced" : "simple";
  let instances = Array.isArray(row.instances)
    ? row.instances.map(parseInstance).filter((item): item is Instance => item !== null)
    : [];
  let active = typeof row.active === "string" ? row.active : null;

  if (instances.length === 0 && typeof row.workspace === "string" && row.workspace.trim()) {
    const workspace = row.workspace;
    instances = [
      {
        id: "apollo",
        name: "apollo",
        everywhere: false,
        workspace,
        config_dir: workspace,
        provider: str(row.provider),
        model: str(row.model),
        permission_profile: str(row.permission_profile, "auto") || "auto",
        color: null,
        pinned: false,
      },
    ];
    active = "apollo";
  }

  if (active && !instances.some((item) => item.id === active)) {
    active = instances[0]?.id ?? null;
  }

  return {
    onboarded: row.onboarded === true && instances.length > 0,
    mode,
    active,
    instances,
  };
}

export function toDesktopJson(state: DesktopState): string {
  const parsed = parseDesktopState(state);
  const body = {
    onboarded: parsed.onboarded,
    mode: parsed.mode,
    active: parsed.active,
    instances: parsed.instances.map((instance) => {
      const out: Record<string, unknown> = {};
      for (const key of INSTANCE_KEYS) out[key] = instance[key];
      return out;
    }),
  };
  return JSON.stringify(body, null, 2);
}

export function activeInstance(state: DesktopState): Instance | null {
  if (!state.onboarded) return null;
  return (
    state.instances.find((item) => item.id === state.active) ??
    state.instances[0] ??
    null
  );
}

export function upsert(state: DesktopState, instance: Instance): DesktopState {
  const instances = state.instances.some((item) => item.id === instance.id)
    ? state.instances.map((item) => (item.id === instance.id ? instance : item))
    : [...state.instances, instance];
  return { ...state, active: instance.id, instances };
}

/** A directory path. URLs are rejected so a host is not stored as a workspace. */
export function validateFolder(path: string): string | null {
  const value = path.trim();
  if (!value) return "a folder needs a path";
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(value)) return "a workspace is a directory, not a url";
  if (value.includes("\0")) return "that path is not a directory";
  return null;
}

export function validateKey(auth: ProviderInfo["auth"], key: string): string | null {
  const value = key.trim();
  if (auth === "api-key" && !value) return "this provider needs a key";
  if (!value) return null;
  if (/[\s"\\]/.test(value) || [...value].some((ch) => ch.charCodeAt(0) < 32)) {
    return "that key contains characters an api key never has";
  }
  return null;
}

export function masked(secret: string): string {
  const n = [...secret].length;
  if (!n) return "";
  return "•".repeat(Math.min(n, 24));
}
