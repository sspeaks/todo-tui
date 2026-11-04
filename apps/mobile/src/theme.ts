import { useColorScheme } from "react-native";

const light = {
  bg: "#ffffff",
  surface: "#f3f4f6",
  text: "#111827",
  dim: "#6b7280",
  border: "#e5e7eb",
  accent: "#2563eb",
  danger: "#dc2626",
  warn: "#b45309",
  ok: "#059669",
  onAccent: "#ffffff",
};

export type Theme = typeof light;

const dark: Theme = {
  bg: "#0b0f14",
  surface: "#161b22",
  text: "#e5e7eb",
  dim: "#9ca3af",
  border: "#262c36",
  accent: "#60a5fa",
  danger: "#f87171",
  warn: "#fbbf24",
  ok: "#34d399",
  onAccent: "#0b0f14",
};

export const useTheme = (): Theme => (useColorScheme() === "dark" ? dark : light);
