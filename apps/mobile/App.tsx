import {
  ChivoMono_400Regular,
  ChivoMono_600SemiBold,
} from "@expo-google-fonts/chivo-mono";
import { useFonts } from "expo-font";
import { StatusBar } from "expo-status-bar";
import { useEffect, useState } from "react";
import { ActivityIndicator, StyleSheet, View } from "react-native";

import { complete } from "./src/chat";
import {
  activeInstance,
  emptyState,
  upsert,
  type DesktopState,
  type Instance,
  type Mode,
} from "./src/config";
import { previewFromHash, sampleLog, sampleMessages, sampleState, type PreviewId } from "./src/preview";
import { providerById } from "./src/providers";
import { loadState, saveState } from "./src/storage";
import { colors } from "./src/theme";
import { Desk, type Bubble, type LogLine } from "./src/ui/Desk";
import { Onboarding, type FinishedSetup, type Step } from "./src/ui/Onboarding";

interface Secret {
  key: string;
  baseUrl: string;
}

function stamp(): string {
  const now = new Date();
  const h = String(now.getHours()).padStart(2, "0");
  const m = String(now.getMinutes()).padStart(2, "0");
  return `${h}:${m}`;
}

export default function App() {
  const [fontsLoaded] = useFonts({
    ChivoMono_400Regular,
    ChivoMono_600SemiBold,
  });
  const [preview, setPreview] = useState<PreviewId | null>(null);
  const [booting, setBooting] = useState(true);
  const [state, setState] = useState<DesktopState>(emptyState());
  const [adding, setAdding] = useState(false);
  const [messages, setMessages] = useState<Bubble[]>([]);
  const [log, setLog] = useState<LogLine[]>([]);
  const [switcherOpen, setSwitcherOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [secrets, setSecrets] = useState<Record<string, Secret>>({});

  useEffect(() => {
    const hash = typeof window === "undefined" ? "" : window.location.hash;
    const shot = previewFromHash(hash);
    setPreview(shot);
    if (shot) {
      setBooting(false);
      return;
    }
    void loadState().then((loaded) => {
      if (loaded) setState(loaded);
      setBooting(false);
    });
  }, []);

  useEffect(() => {
    if (preview || booting || !state.onboarded) return;
    void saveState(state);
  }, [state, preview, booting]);

  function pushLog(line: string) {
    setLog((prev) => [...prev, { t: stamp(), line }]);
  }

  function applySetup(setup: FinishedSetup, first: boolean) {
    const instance: Instance = {
      id: setup.id,
      name: setup.name,
      everywhere: setup.everywhere,
      workspace: setup.workspace,
      config_dir: setup.configDir,
      provider: setup.provider,
      model: setup.model,
      permission_profile: setup.profile,
      color: null,
      pinned: false,
    };
    setState((prev) => {
      const next = upsert(first ? { ...prev, mode: setup.mode, onboarded: true } : { ...prev, onboarded: true }, instance);
      return next;
    });
    if (setup.key || setup.baseUrl) {
      setSecrets((prev) => ({ ...prev, [setup.id]: { key: setup.key, baseUrl: setup.baseUrl } }));
    }
    setAdding(false);
    pushLog(`saved instance ${setup.name}`);
  }

  async function onSend(text: string) {
    const instance = activeInstance(state);
    if (!instance) return;
    const provider = providerById(instance.provider);
    const secret = secrets[instance.id];
    const history = [...messages, { role: "user" as const, text }];
    setMessages(history);
    setBusy(true);
    const result = await complete({
      auth: provider?.auth ?? "oauth",
      baseUrl: provider?.auth === "custom" ? secret?.baseUrl : provider?.baseUrl,
      key: secret?.key ?? "",
      model: instance.model || provider?.defaultModel || "default",
      messages: history.map((message) => ({ role: message.role, text: message.text })),
    });
    setMessages((prev) => [...prev, { role: "assistant", text: result.text, route: result.label }]);
    pushLog(result.label);
    setBusy(false);
  }

  function patchActive(change: (instance: Instance) => Instance) {
    setState((prev) => {
      const current = activeInstance(prev);
      if (!current) return prev;
      const next = change(current);
      return { ...prev, instances: prev.instances.map((item) => (item.id === next.id ? next : item)) };
    });
  }

  if (!fontsLoaded || booting) {
    return (
      <View style={styles.boot}>
        <ActivityIndicator color={colors.accent} />
      </View>
    );
  }

  const previewNode = preview ? renderPreview(preview) : null;

  return (
    <View style={styles.root} nativeID="apollo-ready">
      <StatusBar style="light" />
      {previewNode ??
        (adding || !state.onboarded ? (
          <Onboarding
            purpose={adding ? "new" : "first"}
            takenIds={state.instances.map((item) => item.id)}
            onCancel={adding ? () => setAdding(false) : undefined}
            onDone={(setup) => applySetup(setup, !adding)}
          />
        ) : (
          <Desk
            state={state}
            messages={messages}
            log={log}
            switcherOpen={switcherOpen}
            busy={busy}
            onSend={(text) => void onSend(text)}
            onTogglePin={(id) =>
              setState((prev) => ({
                ...prev,
                instances: prev.instances.map((item) => (item.id === id ? { ...item, pinned: !item.pinned } : item)),
              }))
            }
            onSelect={(id) => setState((prev) => ({ ...prev, active: id }))}
            onNew={() => setAdding(true)}
            onMode={(mode: Mode) => {
              setState((prev) => ({ ...prev, mode }));
              pushLog(`mode ${mode}`);
            }}
            onProfile={(id) => {
              patchActive((instance) => ({ ...instance, permission_profile: id }));
              pushLog(`profile ${id}`);
            }}
            onOpenSwitcher={setSwitcherOpen}
          />
        ))}
    </View>
  );
}

function renderPreview(id: PreviewId) {
  if (id === "simple" || id === "advanced" || id === "instances") {
    const mode = id === "advanced" ? "advanced" : "simple";
    return (
      <Desk
        state={sampleState(mode)}
        messages={sampleMessages}
        log={sampleLog}
        switcherOpen={id === "instances"}
        busy={false}
        onSend={() => undefined}
        onTogglePin={() => undefined}
        onSelect={() => undefined}
        onNew={() => undefined}
        onMode={() => undefined}
        onProfile={() => undefined}
        onOpenSwitcher={() => undefined}
      />
    );
  }
  const step = id as Step;
  return (
    <Onboarding
      purpose="first"
      startStep={step}
      takenIds={[]}
      previewProbe={
        step === "test"
          ? {
              label: "offline mock · no key in this session",
              text: "offline mock · no key in this session. nothing was sent.",
            }
          : null
      }
      onDone={() => undefined}
    />
  );
}


const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: colors.bg },
  boot: { flex: 1, backgroundColor: colors.bg, alignItems: "center", justifyContent: "center" },
});
