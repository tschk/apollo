import { useState } from "react";
import {
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  View,
} from "react-native";

import { activeInstance, rosterOrder, scopeLabel, toDesktopJson, type DesktopState, type Instance, type Mode } from "../config";
import { PROFILES } from "../providers";
import { colors, font, space } from "../theme";

export interface Bubble {
  role: "user" | "assistant";
  text: string;
  route?: string;
}

export interface LogLine {
  t: string;
  line: string;
}

export function Desk({
  state,
  messages,
  log,
  switcherOpen,
  busy,
  onSend,
  onTogglePin,
  onSelect,
  onNew,
  onMode,
  onProfile,
  onOpenSwitcher,
}: {
  state: DesktopState;
  messages: Bubble[];
  log: LogLine[];
  switcherOpen: boolean;
  busy: boolean;
  onSend: (text: string) => void;
  onTogglePin: (id: string) => void;
  onSelect: (id: string) => void;
  onNew: () => void;
  onMode: (mode: Mode) => void;
  onProfile: (id: string) => void;
  onOpenSwitcher: (open: boolean) => void;
}) {
  const [draft, setDraft] = useState("");
  const current = activeInstance(state);
  const roster = rosterOrder(state.instances);
  const advanced = state.mode === "advanced";

  function send() {
    const text = draft.trim();
    if (!text || busy) return;
    setDraft("");
    onSend(text);
  }

  return (
    <View style={styles.screen}>
      <View style={styles.header}>
        <Text style={styles.word}>apollo</Text>
        <Pressable onPress={() => onMode(advanced ? "simple" : "advanced")}>
          <Text style={styles.mode}>{state.mode}</Text>
        </Pressable>
      </View>

      <ScrollView contentContainerStyle={styles.scroll} keyboardShouldPersistTaps="handled">
        {advanced ? (
          <View>
            <Text style={styles.section}>instances</Text>
            {roster.map((instance) => (
              <InstanceRow
                key={instance.id}
                instance={instance}
                active={instance.id === current?.id}
                onPress={() => onSelect(instance.id)}
                onPin={() => onTogglePin(instance.id)}
              />
            ))}
            <Pressable onPress={onNew} style={styles.newRow}>
              <Text style={styles.newText}>+ new instance</Text>
            </Pressable>
            <Text style={styles.section}>tools</Text>
            <View style={styles.profiles}>
              {PROFILES.map((profile) => (
                <Pressable
                  key={profile.id}
                  onPress={() => current && onProfile(profile.id)}
                  style={[styles.chip, current?.permission_profile === profile.id ? styles.chipOn : null]}
                >
                  <Text style={[styles.chipText, current?.permission_profile === profile.id ? styles.chipTextOn : null]}>
                    {profile.label}
                  </Text>
                </Pressable>
              ))}
            </View>
          </View>
        ) : (
          <Pressable onPress={() => onOpenSwitcher(true)} style={styles.switcher}>
            <View>
              <Text style={styles.switcherName}>{current?.name ?? "no instance"}</Text>
              <Text style={styles.switcherMeta}>
                {current ? `${current.provider} · ${scopeLabel(current)}` : "finish setup"}
              </Text>
            </View>
            <Text style={styles.switcherCue}>switch</Text>
          </Pressable>
        )}

        <Text style={styles.section}>chat</Text>
        {messages.length === 0 ? (
          <Text style={styles.empty}>a prompt stays on this device. oauth sign-in is not replayed from here.</Text>
        ) : null}
        {messages.map((message, index) => (
          <View key={`${message.role}-${index}`} style={[styles.bubble, message.role === "user" ? styles.user : styles.assistant]}>
            <Text style={styles.bubbleText}>{message.text}</Text>
            {message.route ? <Text style={styles.route}>{message.route}</Text> : null}
          </View>
        ))}

        {advanced ? (
          <View>
            <Text style={styles.section}>log</Text>
            {log.length === 0 ? <Text style={styles.empty}>quiet.</Text> : null}
            {log.map((line, index) => (
              <Text key={`${line.t}-${index}`} style={styles.logLine}>
                {line.t}  {line.line}
              </Text>
            ))}
            <Text style={styles.section}>desktop.json</Text>
            <Text style={styles.json}>{toDesktopJson(state)}</Text>
          </View>
        ) : null}
      </ScrollView>

      <View style={styles.composer}>
        <TextInput
          value={draft}
          onChangeText={setDraft}
          placeholder={current ? `message ${current.name}` : "no instance"}
          placeholderTextColor={colors.ghost}
          style={styles.input}
          autoCorrect={false}
          editable={!busy}
        />
        <Pressable onPress={send} style={styles.send}>
          <Text style={styles.sendText}>{busy ? "…" : "send"}</Text>
        </Pressable>
      </View>

      {switcherOpen && !advanced ? (
        <View style={styles.sheetWrap}>
          <Pressable style={styles.scrim} onPress={() => onOpenSwitcher(false)} />
          <View style={styles.sheet}>
            <Text style={styles.section}>instances</Text>
            {roster.map((instance) => (
              <InstanceRow
                key={instance.id}
                instance={instance}
                active={instance.id === current?.id}
                onPress={() => {
                  onSelect(instance.id);
                  onOpenSwitcher(false);
                }}
                onPin={() => onTogglePin(instance.id)}
              />
            ))}
            <Pressable
              onPress={() => {
                onOpenSwitcher(false);
                onNew();
              }}
              style={styles.newRow}
            >
              <Text style={styles.newText}>+ new instance</Text>
            </Pressable>
          </View>
        </View>
      ) : null}
    </View>
  );
}

function InstanceRow({
  instance,
  active,
  onPress,
  onPin,
}: {
  instance: Instance;
  active: boolean;
  onPress: () => void;
  onPin: () => void;
}) {
  return (
    <View style={[styles.row, active ? styles.rowOn : null]}>
      <Pressable onPress={onPress} style={styles.rowMain}>
        <Text style={styles.rowName}>{instance.name}</Text>
        <Text style={styles.rowMeta}>
          {instance.provider} · {instance.model || "model unset"} · {scopeLabel(instance)}
        </Text>
      </Pressable>
      <Pressable onPress={onPin} style={styles.pin}>
        <Text style={styles.pinText}>{instance.pinned ? "pinned" : "pin"}</Text>
      </Pressable>
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg },
  header: {
    paddingHorizontal: space[5],
    paddingTop: space[4],
    paddingBottom: space[3],
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "baseline",
    borderBottomWidth: 1,
    borderBottomColor: colors.surface2,
  },
  word: { fontFamily: font.semibold, color: colors.accent, fontSize: 22, letterSpacing: -0.4 },
  mode: { fontFamily: font.regular, color: colors.muted, fontSize: 12 },
  scroll: { padding: space[5], paddingBottom: space[6], gap: space[2] },
  section: {
    fontFamily: font.regular,
    color: colors.ghost,
    fontSize: 11,
    marginTop: space[3],
    marginBottom: space[2],
  },
  switcher: {
    borderWidth: 1,
    borderColor: colors.border,
    backgroundColor: colors.surface,
    padding: space[4],
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "center",
  },
  switcherName: { fontFamily: font.semibold, color: colors.accent, fontSize: 16 },
  switcherMeta: { fontFamily: font.regular, color: colors.muted, fontSize: 12, marginTop: 4 },
  switcherCue: { fontFamily: font.regular, color: colors.soft, fontSize: 12 },
  row: {
    flexDirection: "row",
    alignItems: "center",
    borderWidth: 1,
    borderColor: colors.border,
    backgroundColor: colors.surface,
    marginBottom: space[2],
  },
  rowOn: { borderColor: colors.accent },
  rowMain: { flex: 1, padding: space[3] },
  rowName: { fontFamily: font.semibold, color: colors.accent, fontSize: 14 },
  rowMeta: { fontFamily: font.regular, color: colors.muted, fontSize: 11, marginTop: 3 },
  pin: { paddingHorizontal: space[3], paddingVertical: space[3] },
  pinText: { fontFamily: font.regular, color: colors.soft, fontSize: 11 },
  newRow: { paddingVertical: space[3] },
  newText: { fontFamily: font.regular, color: colors.soft, fontSize: 13 },
  profiles: { flexDirection: "row", flexWrap: "wrap", gap: space[2] },
  chip: { borderWidth: 1, borderColor: colors.border, paddingHorizontal: space[3], paddingVertical: space[2] },
  chipOn: { backgroundColor: colors.accent, borderColor: colors.accent },
  chipText: { fontFamily: font.regular, color: colors.text, fontSize: 12 },
  chipTextOn: { color: colors.accentFg },
  empty: { fontFamily: font.regular, color: colors.muted, fontSize: 13, lineHeight: 18 },
  bubble: { padding: space[3], marginBottom: space[2], maxWidth: "100%" },
  user: { backgroundColor: colors.surface2, alignSelf: "flex-end" },
  assistant: { backgroundColor: colors.surface, borderWidth: 1, borderColor: colors.border, alignSelf: "flex-start" },
  bubbleText: { fontFamily: font.regular, color: colors.text, fontSize: 14, lineHeight: 20 },
  route: { fontFamily: font.regular, color: colors.success, fontSize: 10, marginTop: space[2] },
  logLine: { fontFamily: font.regular, color: colors.soft, fontSize: 12, lineHeight: 18 },
  json: { fontFamily: font.regular, color: colors.muted, fontSize: 10, lineHeight: 14 },
  composer: {
    flexDirection: "row",
    gap: space[2],
    padding: space[4],
    borderTopWidth: 1,
    borderTopColor: colors.surface2,
    alignItems: "center",
  },
  input: {
    flex: 1,
    fontFamily: font.regular,
    color: colors.text,
    borderWidth: 1,
    borderColor: colors.border,
    backgroundColor: colors.surface,
    paddingHorizontal: space[3],
    paddingVertical: space[3],
    fontSize: 14,
  },
  send: { backgroundColor: colors.accent, paddingHorizontal: space[4], paddingVertical: space[3] },
  sendText: { fontFamily: font.semibold, color: colors.accentFg, fontSize: 13 },
  sheetWrap: { position: "absolute", top: 0, right: 0, bottom: 0, left: 0, justifyContent: "flex-end" },
  scrim: { position: "absolute", top: 0, right: 0, bottom: 0, left: 0, backgroundColor: "#00000088" },
  sheet: {
    backgroundColor: colors.bg,
    borderTopWidth: 1,
    borderTopColor: colors.border,
    padding: space[5],
    paddingBottom: space[6],
  },
});
