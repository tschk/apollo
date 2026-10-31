import AsyncStorage from "@react-native-async-storage/async-storage";

import { parseDesktopState, toDesktopJson, type DesktopState } from "./config";

/** App-storage copy of the desktop.json document. Not `~/.apollo`. */
export const STORAGE_KEY = "apollo.desktop.json";

export async function loadState(): Promise<DesktopState | null> {
  const raw = await AsyncStorage.getItem(STORAGE_KEY);
  if (!raw) return null;
  try {
    return parseDesktopState(JSON.parse(raw));
  } catch {
    return null;
  }
}

export async function saveState(state: DesktopState): Promise<void> {
  await AsyncStorage.setItem(STORAGE_KEY, toDesktopJson(state));
}
