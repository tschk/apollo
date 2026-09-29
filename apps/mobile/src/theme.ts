/**
 * Apollo desktop tokens (ui/src/theme.rs on the gui branch): zinc-950,
 * Chivo Mono. Crepuscularity is a GPUI layer and does not ship a phone
 * target, so these constants are the React Native stand-in.
 */
import type { TextStyle, ViewStyle } from "react-native";

export const colors = {
  bg: "#09090b",
  surface: "#18181b",
  surface2: "#27272a",
  border: "#3f3f46",
  text: "#d4d4d8",
  soft: "#a1a1aa",
  muted: "#71717a",
  ghost: "#52525b",
  accent: "#fafafa",
  accentFg: "#09090b",
  success: "#34d399",
  warn: "#fbbf24",
  danger: "#f87171",
} as const;

export const space = {
  1: 4,
  2: 8,
  3: 12,
  4: 16,
  5: 24,
  6: 32,
} as const;

export const font = {
  regular: "ChivoMono_400Regular",
  semibold: "ChivoMono_600SemiBold",
} as const;

export const type = {
  body: { fontFamily: font.regular, fontSize: 14, lineHeight: 20, color: colors.text } satisfies TextStyle,
  small: { fontFamily: font.regular, fontSize: 12, lineHeight: 16, color: colors.muted } satisfies TextStyle,
  title: { fontFamily: font.semibold, fontSize: 28, lineHeight: 34, color: colors.accent, letterSpacing: -0.5 } satisfies TextStyle,
  label: { fontFamily: font.semibold, fontSize: 13, lineHeight: 18, color: colors.text } satisfies TextStyle,
};

export const hairline: ViewStyle = {
  borderWidth: 1,
  borderColor: colors.border,
};
