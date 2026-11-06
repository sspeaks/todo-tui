// The paired server and token, kept in the Keychain / Keystore.
import type { Pairing } from "@todo-tui/client/portable";
import * as SecureStore from "expo-secure-store";

const KEY = "todo-tui.pairing";

export async function loadPairing(): Promise<Pairing | undefined> {
  const raw = await SecureStore.getItemAsync(KEY);
  if (!raw) return undefined;
  try {
    const p = JSON.parse(raw) as Partial<Pairing>;
    return typeof p.url === "string" && typeof p.token === "string" ? { url: p.url, token: p.token } : undefined;
  } catch {
    return undefined;
  }
}

export const savePairing = (p: Pairing): Promise<void> =>
  SecureStore.setItemAsync(KEY, JSON.stringify(p), { keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK });

export const clearPairing = (): Promise<void> => SecureStore.deleteItemAsync(KEY);
