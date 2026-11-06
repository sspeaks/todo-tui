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
  /** Behind grouped cards, as iOS settings. */
  grouped: "#f2f2f7",
  card: "#ffffff",
  /** An unselected choice. */
  chip: "#eef0f4",
  /** Priority +1 … +5. */
  p1: "#ef4444",
  p2: "#f97316",
  p3: "#eab308",
  p4: "#3b82f6",
  p5: "#9ca3af",
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
  grouped: "#000000",
  card: "#16181d",
  chip: "#252a33",
  p1: "#f87171",
  p2: "#fb923c",
  p3: "#facc15",
  p4: "#60a5fa",
  p5: "#9ca3af",
};

export const useTheme = (): Theme => (useColorScheme() === "dark" ? dark : light);
