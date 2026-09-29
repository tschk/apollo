import { useMemo, useState } from "react";
import {
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  View,
} from "react-native";

import { complete } from "../chat";
import {
  instanceId,
  masked,
  providerName,
  resolveScope,
  validateFolder,
  validateKey,
  type Mode,
} from "../config";
import {
  KEY_PROVIDERS,
  OAUTH_PROVIDERS,
  PROFILES,
  providerById,
  type ProviderInfo,
} from "../providers";
import { colors, font, space } from "../theme";

export type Step = "welcome" | "provider" | "workspace" | "permissions" | "test" | "mode";

const FIRST_RUN: Step[] = ["welcome", "provider", "workspace", "permissions", "test", "mode"];
const NEW_INSTANCE: Step[] = ["provider", "workspace", "permissions", "test"];

export interface FinishedSetup {
  id: string;
  name: string;
  everywhere: boolean;
  workspace: string;
  configDir: string;
  provider: string;
  model: string;
  profile: string;
  mode: Mode;
  key: string;
  baseUrl: string;
}

interface Probe {
  label: string;
  text: string;
}

export function Onboarding({
  purpose,
  startStep,
  takenIds,
  previewProbe,
  onCancel,
  onDone,
}: {
  purpose: "first" | "new";
  startStep?: Step;
  takenIds: string[];
  previewProbe?: Probe | null;
  onCancel?: () => void;
  onDone: (setup: FinishedSetup) => void;
}) {
  const steps = purpose === "first" ? FIRST_RUN : NEW_INSTANCE;
  const [step, setStep] = useState<Step>(startStep ?? steps[0]);
  const [providerId, setProviderId] = useState("chatgpt");
  const [menuOpen, setMenuOpen] = useState(false);
  const [key, setKey] = useState("");
  const [customName, setCustomName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [name, setName] = useState(purpose === "new" ? "" : "apollo");
  const [everywhere, setEverywhere] = useState(false);
  const [folder, setFolder] = useState("~/src/apollo");
  const [home, setHome] = useState("");
  const [profile, setProfile] = useState("auto");
  const [prompt, setPrompt] = useState("say hello in one line");
  const [probe, setProbe] = useState<Probe | null>(previewProbe ?? null);
  const [running, setRunning] = useState(false);
  const [mode, setMode] = useState<Mode>("simple");
  const [error, setError] = useState<string | null>(null);

  const provider = providerById(providerId) ?? OAUTH_PROVIDERS[0];
  const index = Math.max(0, steps.indexOf(step));
  const resolvedModel = model.trim() || provider.defaultModel;

  const draftId = useMemo(() => instanceId(name, takenIds), [name, takenIds]);

  function go(next: number) {
    const target = steps[next];
    if (target) setStep(target);
  }

  function selectProvider(next: ProviderInfo) {
    setProviderId(next.id);
    setError(null);
    if (next.auth !== "custom") setMenuOpen(false);
  }

  function validateCurrent(): string | null {
    if (step === "provider") {
      const keyError = validateKey(provider.auth, key);
      if (keyError) return keyError;
      if (provider.auth === "custom" && !baseUrl.trim()) return "a custom endpoint needs a base url";
      if (provider.auth === "custom" && !/^[a-z][a-z0-9+.-]*:\/\//i.test(baseUrl.trim())) {
        return "the base url should be an http(s) endpoint";
      }
      if (!resolvedModel) return "name a model";
    }
    if (step === "workspace") {
      if (!name.trim()) return "name this instance";
      if (!everywhere) {
        const folderError = validateFolder(folder);
        if (folderError) return folderError;
      }
    }
    return null;
  }

  async function runProbe() {
    setRunning(true);
    setError(null);
    const result = await complete({
      auth: provider.auth,
      baseUrl: provider.auth === "custom" ? baseUrl : provider.baseUrl,
      key,
      model: resolvedModel || "default",
      messages: [{ role: "user", text: prompt.trim() || "say hello in one line" }],
    });
    setProbe({ label: result.label, text: result.text });
    setRunning(false);
  }

  function finish() {
    const problem = validateCurrent();
    if (problem) {
      setError(problem);
      return;
    }
    const scope = resolveScope({
      everywhere,
      folder,
      instanceId: draftId,
      home,
    });
    onDone({
      id: draftId,
      name: name.trim() || draftId,
      everywhere: scope.everywhere,
      workspace: scope.workspace,
      configDir: scope.config_dir,
      provider: providerName(provider, customName, takenIds),
      model: resolvedModel,
      profile,
      mode,
      key: key.trim(),
      baseUrl: provider.auth === "custom" ? baseUrl.trim() : provider.baseUrl ?? "",
    });
  }

  function next() {
    if (step === "mode" || (purpose === "new" && step === "test")) {
      finish();
      return;
    }
    const problem = validateCurrent();
    if (problem) {
      setError(problem);
      return;
    }
    setError(null);
    go(index + 1);
  }

  const last = index === steps.length - 1;

  return (
    <View style={styles.screen}>
      <View style={styles.track}>
        <View style={[styles.bar, { width: `${((index + 1) / steps.length) * 100}%` }]} />
      </View>
      <ScrollView contentContainerStyle={styles.scroll} keyboardShouldPersistTaps="handled">
        {step === "welcome" ? <Welcome /> : null}
        {step === "provider" ? (
          <ProviderStep
            provider={provider}
            menuOpen={menuOpen}
            customName={customName}
            baseUrl={baseUrl}
            model={model}
            secret={key}
            onToggleMenu={() => setMenuOpen((open) => !open)}
            onSelect={selectProvider}
            onCustomName={setCustomName}
            onBaseUrl={setBaseUrl}
            onModel={setModel}
            onSecret={setKey}
          />
        ) : null}
        {step === "workspace" ? (
          <WorkspaceStep
            name={name}
            everywhere={everywhere}
            folder={folder}
            home={home}
            onName={setName}
            onEverywhere={setEverywhere}
            onFolder={setFolder}
            onHome={setHome}
          />
        ) : null}
        {step === "permissions" ? <PermissionsStep profile={profile} onProfile={setProfile} /> : null}
        {step === "test" ? (
          <TestStep
            prompt={prompt}
            probe={probe}
            running={running}
            onPrompt={setPrompt}
            onRun={() => void runProbe()}
          />
        ) : null}
        {step === "mode" ? <ModeStep mode={mode} onMode={setMode} /> : null}
        {error ? <Text style={styles.error}>{error}</Text> : null}
      </ScrollView>
      <View style={styles.footer}>
        <Text style={styles.count}>
          {index + 1} / {steps.length}
        </Text>
        <View style={styles.footerActions}>
          {index === 0 && onCancel ? (
            <Pressable onPress={onCancel} style={styles.ghost}>
              <Text style={styles.ghostText}>cancel</Text>
            </Pressable>
          ) : null}
          {index > 0 ? (
            <Pressable
              onPress={() => {
                setError(null);
                go(index - 1);
              }}
              style={styles.ghost}
            >
              <Text style={styles.ghostText}>back</Text>
            </Pressable>
          ) : null}
          <Pressable onPress={next} style={styles.primary}>
            <Text style={styles.primaryText}>{last ? (purpose === "first" ? "open the desk" : "save instance") : "continue"}</Text>
          </Pressable>
        </View>
      </View>
    </View>
  );
}

function Welcome() {
  return (
    <View>
      <Text style={styles.kicker}>welcome</Text>
      <Text style={styles.title}>meet apollo</Text>
      <Text style={styles.lede}>
        the agent that comes with you. a model, a place to work, then the conversation.
      </Text>
      <View style={styles.stack}>
        {["sign in, or a key", "one folder, or everywhere", "what it may do", "a test prompt, then simple or advanced"].map(
          (line, i) => (
            <View key={line} style={styles.stepRow}>
              <Text style={styles.stepIndex}>0{i + 1}</Text>
              <Text style={styles.stepText}>{line}</Text>
            </View>
          ),
        )}
      </View>
    </View>
  );
}

function ProviderStep({
  provider,
  menuOpen,
  customName,
  baseUrl,
  model,
  secret,
  onToggleMenu,
  onSelect,
  onCustomName,
  onBaseUrl,
  onModel,
  onSecret,
}: {
  provider: ProviderInfo;
  menuOpen: boolean;
  customName: string;
  baseUrl: string;
  model: string;
  secret: string;
  onToggleMenu: () => void;
  onSelect: (provider: ProviderInfo) => void;
  onCustomName: (value: string) => void;
  onBaseUrl: (value: string) => void;
  onModel: (value: string) => void;
  onSecret: (value: string) => void;
}) {
  return (
    <View>
      <Text style={styles.kicker}>provider</Text>
      <Text style={styles.title}>connect a model</Text>
      <Text style={styles.hint}>an account, or a key. the key never goes in the shared config.</Text>
      {OAUTH_PROVIDERS.map((item) => (
        <Pressable
          key={item.id}
          onPress={() => onSelect(item)}
          style={[styles.card, provider.id === item.id ? styles.cardOn : null]}
        >
          <Text style={styles.cardTitle}>{item.label}</Text>
          <Text style={styles.cardBody}>{item.blurb}</Text>
        </Pressable>
      ))}
      {!menuOpen && provider.auth !== "oauth" ? (
        <Pressable onPress={onToggleMenu} style={[styles.card, styles.cardOn]}>
          <Text style={styles.cardTitle}>{provider.label}</Text>
          <Text style={styles.cardBody}>{provider.blurb}</Text>
        </Pressable>
      ) : null}
      <Pressable onPress={onToggleMenu} style={styles.menuButton}>
        <Text style={styles.menuButtonText}>{menuOpen ? "hide other providers" : "other providers"}</Text>
      </Pressable>
      {menuOpen
        ? KEY_PROVIDERS.map((item) => (
            <Pressable
              key={item.id}
              onPress={() => onSelect(item)}
              style={[styles.card, provider.id === item.id ? styles.cardOn : null]}
            >
              <Text style={styles.cardTitle}>{item.label}</Text>
              <Text style={styles.cardBody}>{item.blurb}</Text>
            </Pressable>
          ))
        : null}
      <Text style={styles.note}>{provider.note}</Text>
      {provider.auth === "custom" ? (
        <>
          <Field label="name" value={customName} onChange={onCustomName} placeholder="lab" />
          <Field label="base url" value={baseUrl} onChange={onBaseUrl} placeholder="https://host/v1" autoCapitalize="none" />
        </>
      ) : null}
      {provider.auth === "api-key" || provider.auth === "custom" ? (
        <Field
          label={provider.envVar ?? "api key"}
          value={secret}
          onChange={onSecret}
          placeholder={provider.auth === "custom" ? "optional" : "for this session"}
          secure
          autoCapitalize="none"
        />
      ) : null}
      <Field
        label="model"
        value={model}
        onChange={onModel}
        placeholder={provider.defaultModel || "model id"}
        autoCapitalize="none"
      />
    </View>
  );
}

function WorkspaceStep({
  name,
  everywhere,
  folder,
  home,
  onName,
  onEverywhere,
  onFolder,
  onHome,
}: {
  name: string;
  everywhere: boolean;
  folder: string;
  home: string;
  onName: (value: string) => void;
  onEverywhere: (value: boolean) => void;
  onFolder: (value: string) => void;
  onHome: (value: string) => void;
}) {
  return (
    <View>
      <Text style={styles.kicker}>scope</Text>
      <Text style={styles.title}>where it works</Text>
      <Text style={styles.hint}>the place this agent is allowed to touch.</Text>
      <Field label="instance name" value={name} onChange={onName} placeholder="apollo" autoCapitalize="none" />
      <Pressable onPress={() => onEverywhere(false)} style={[styles.card, !everywhere ? styles.cardOn : null]}>
        <Text style={styles.cardTitle}>one folder</Text>
        <Text style={styles.cardBody}>setup stays in that folder.</Text>
      </Pressable>
      <Pressable onPress={() => onEverywhere(true)} style={[styles.card, everywhere ? styles.cardOn : null]}>
        <Text style={styles.cardTitle}>everywhere</Text>
        <Text style={styles.cardBody}>home, not one folder. setup goes under ~/.apollo/instances.</Text>
      </Pressable>
      {everywhere ? (
        <Field label="home folder" value={home} onChange={onHome} placeholder="~" autoCapitalize="none" />
      ) : (
        <Field label="folder" value={folder} onChange={onFolder} placeholder="~/src/apollo" autoCapitalize="none" />
      )}
    </View>
  );
}

function PermissionsStep({ profile, onProfile }: { profile: string; onProfile: (id: string) => void }) {
  return (
    <View>
      <Text style={styles.kicker}>permissions</Text>
      <Text style={styles.title}>what it may do</Text>
      <Text style={styles.hint}>what it may do without asking. set this before the first real turn.</Text>
      {PROFILES.map((item) => (
        <Pressable key={item.id} onPress={() => onProfile(item.id)} style={[styles.card, profile === item.id ? styles.cardOn : null]}>
          <Text style={styles.cardTitle}>{item.label}</Text>
          <Text style={styles.cardBody}>{item.detail}</Text>
        </Pressable>
      ))}
    </View>
  );
}

function TestStep({
  prompt,
  probe,
  running,
  onPrompt,
  onRun,
}: {
  prompt: string;
  probe: Probe | null;
  running: boolean;
  onPrompt: (value: string) => void;
  onRun: () => void;
}) {
  return (
    <View>
      <Text style={styles.kicker}>test</Text>
      <Text style={styles.title}>a test prompt</Text>
      <Text style={styles.hint}>send one line. without a key, the reply is a stand-in, and it says so.</Text>
      <Field label="prompt" value={prompt} onChange={onPrompt} placeholder="say hello in one line" />
      <Pressable onPress={onRun} style={styles.primary}>
        <Text style={styles.primaryText}>{running ? "running" : "run"}</Text>
      </Pressable>
      {probe ? (
        <View style={styles.probe}>
          <Text style={styles.probeLabel}>{probe.label}</Text>
          <Text style={styles.probeText}>{probe.text}</Text>
        </View>
      ) : null}
    </View>
  );
}

function ModeStep({ mode, onMode }: { mode: Mode; onMode: (mode: Mode) => void }) {
  return (
    <View>
      <Text style={styles.kicker}>ready</Text>
      <Text style={styles.title}>how much app</Text>
      <Pressable onPress={() => onMode("simple")} style={[styles.card, mode === "simple" ? styles.cardOn : null]}>
        <Text style={styles.cardTitle}>simple</Text>
        <Text style={styles.cardBody}>chat, and a switcher. nothing else.</Text>
      </Pressable>
      <Pressable onPress={() => onMode("advanced")} style={[styles.card, mode === "advanced" ? styles.cardOn : null]}>
        <Text style={styles.cardTitle}>advanced</Text>
        <Text style={styles.cardBody}>the roster, tools, and a log. switch any time.</Text>
      </Pressable>
    </View>
  );
}

function Field({
  label,
  value,
  onChange,
  placeholder,
  secure,
  autoCapitalize,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  secure?: boolean;
  autoCapitalize?: "none" | "sentences";
}) {
  return (
    <View style={styles.field}>
      <Text style={styles.fieldLabel}>{label}</Text>
      <TextInput
        value={secure ? masked(value) : value}
        onChangeText={(next) => {
          if (!secure) {
            onChange(next);
            return;
          }
          if (next.length < masked(value).length) onChange(value.slice(0, -1));
          else onChange(value + next.slice(masked(value).length));
        }}
        placeholder={placeholder}
        placeholderTextColor={colors.ghost}
        autoCapitalize={autoCapitalize ?? "sentences"}
        autoCorrect={false}
        style={styles.input}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg },
  track: { height: 2, backgroundColor: colors.surface2 },
  bar: { height: 2, backgroundColor: colors.accent },
  scroll: { padding: space[5], paddingBottom: space[6], gap: space[3] },
  kicker: { fontFamily: font.regular, color: colors.muted, fontSize: 12, marginBottom: space[2] },
  title: { fontFamily: font.semibold, color: colors.accent, fontSize: 28, letterSpacing: -0.4, marginBottom: space[3] },
  lede: { fontFamily: font.regular, color: colors.text, fontSize: 15, lineHeight: 22 },
  hint: { fontFamily: font.regular, color: colors.muted, fontSize: 12, lineHeight: 17, marginBottom: space[2] },
  stack: { marginTop: space[5], gap: space[3] },
  stepRow: { flexDirection: "row", gap: space[3], alignItems: "center" },
  stepIndex: { fontFamily: font.regular, color: colors.ghost, fontSize: 12, width: 24 },
  stepText: { fontFamily: font.regular, color: colors.text, fontSize: 14, flex: 1 },
  card: {
    borderWidth: 1,
    borderColor: colors.border,
    backgroundColor: colors.surface,
    padding: space[4],
    marginBottom: space[2],
  },
  cardOn: { borderColor: colors.accent },
  cardTitle: { fontFamily: font.semibold, color: colors.accent, fontSize: 14, marginBottom: 4 },
  cardBody: { fontFamily: font.regular, color: colors.soft, fontSize: 12, lineHeight: 17 },
  note: { fontFamily: font.regular, color: colors.muted, fontSize: 12, lineHeight: 17, marginVertical: space[3] },
  menuButton: { paddingVertical: space[3] },
  menuButtonText: { fontFamily: font.regular, color: colors.soft, fontSize: 13 },
  field: { marginBottom: space[3] },
  fieldLabel: { fontFamily: font.regular, color: colors.muted, fontSize: 11, marginBottom: 6 },
  input: {
    fontFamily: font.regular,
    color: colors.text,
    borderWidth: 1,
    borderColor: colors.border,
    backgroundColor: colors.surface,
    paddingHorizontal: space[3],
    paddingVertical: space[3],
    fontSize: 14,
  },
  probe: { marginTop: space[4], backgroundColor: colors.surface, padding: space[4], borderWidth: 1, borderColor: colors.border },
  probeLabel: { fontFamily: font.regular, color: colors.success, fontSize: 11, marginBottom: space[2] },
  probeText: { fontFamily: font.regular, color: colors.text, fontSize: 14, lineHeight: 20 },
  error: { fontFamily: font.regular, color: colors.danger, fontSize: 12, marginTop: space[2] },
  footer: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    paddingHorizontal: space[5],
    paddingVertical: space[4],
    borderTopWidth: 1,
    borderTopColor: colors.surface2,
  },
  count: { fontFamily: font.regular, color: colors.ghost, fontSize: 12 },
  footerActions: { flexDirection: "row", gap: space[2] },
  primary: { backgroundColor: colors.accent, paddingHorizontal: space[4], paddingVertical: space[3] },
  primaryText: { fontFamily: font.semibold, color: colors.accentFg, fontSize: 13 },
  ghost: { paddingHorizontal: space[3], paddingVertical: space[3] },
  ghostText: { fontFamily: font.regular, color: colors.soft, fontSize: 13 },
});
