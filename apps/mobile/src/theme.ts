// The phone's look: a terminal's (the TUI's cyan, JetBrains Mono for the
// accents) in light and dark, following the system. Titles and notes stay
// in the system font, for reading; mono is for what the TUI would print in
// a column: dates, priorities, labels, counts, ids and headings.
import { Priority } from "@todo-tui/client/portable";
import { Platform, type TextStyle, useColorScheme } from "react-native";

const light = {
  /** Paper. */
  bg: "#fafaf9",
  /** Inputs, banners, an unselected tab. */
  surface: "#f0efed",
  text: "#1c1917",
  dim: "#62625d",
  border: "#e4e3e0",
  /** The TUI's cyan, dark enough for text on paper (AA on paper, on its own tint, and under white). */
  accent: "#0c6b85",
  /** The accent behind text: a selected tab, a label chip. */
  accentSoft: "rgba(12, 107, 133, 0.10)",
  danger: "#dc2626",
  warn: "#a24a06",
  ok: "#047857",
  onAccent: "#ffffff",
  /** Text and icons on a swipe's action colours (ok, danger). */
  onAction: "#ffffff",
  /** Behind grouped cards, as iOS settings. */
  grouped: "#f0efed",
  card: "#ffffff",
  /** An unselected choice. */
  chip: "#ebeae7",
  /** Priority +1 … +5: the row's edge, the add sheet's dots. */
  p1: "#ef4444",
  p2: "#f97316",
  p3: "#eab308",
  p4: "#3b82f6",
  p5: "#9ca3af",
};

type Palette = typeof light;

const dark: Palette = {
  bg: "#0d1117",
  surface: "#161b22",
  text: "#e6edf3",
  dim: "#8b949e",
  border: "#30363d",
  accent: "#22d3ee",
  accentSoft: "rgba(34, 211, 238, 0.14)",
  danger: "#f87171",
  warn: "#fbbf24",
  ok: "#34d399",
  onAccent: "#0d1117",
  // Dark mode's ok and danger are light: dark text on them.
  onAction: "#0d1117",
  grouped: "#0d1117",
  card: "#161b22",
  chip: "#21262d",
  p1: "#f87171",
  p2: "#fb923c",
  p3: "#facc15",
  p4: "#60a5fa",
  p5: "#9ca3af",
};

/** The mono faces: spread into a text style (`...t.mono`). */
interface Fonts {
  mono: TextStyle;
  monoBold: TextStyle;
}

/** The faces the root layout loads (src/app/_layout.tsx), by the names it loads them under. */
const BUNDLED: Fonts = { mono: { fontFamily: "JetBrainsMono_400Regular" }, monoBold: { fontFamily: "JetBrainsMono_700Bold" } };
const SYSTEM_MONO = Platform.select({ ios: "Menlo", default: "monospace" });
const SYSTEM: Fonts = { mono: { fontFamily: SYSTEM_MONO }, monoBold: { fontFamily: SYSTEM_MONO, fontWeight: "700" } };

export type Theme = Palette & Fonts;

const build = (fonts: Fonts) => ({ light: { ...light, ...fonts }, dark: { ...dark, ...fonts } });
let themes = build(BUNDLED);

/** The bundled mono did not load: the system's mono from now on. Called before the screens render. */
export function fallBackToSystemMono(): void {
  if (themes.light.mono !== SYSTEM.mono) themes = build(SYSTEM);
}

export const useTheme = (): Theme => (useColorScheme() === "dark" ? themes.dark : themes.light);

/** Font sizes: a title, body text, a row's meta line, a caption. */
export const size = { title: 22, body: 16, meta: 13, caption: 12 } as const;

/** Spacing steps. */
export const space = { xs: 4, s: 8, m: 12, l: 16, xl: 24 } as const;

const priorityColors: Partial<Record<Priority, keyof Palette>> = {
  [Priority.HIGHEST]: "p1",
  [Priority.HIGH]: "p2",
  [Priority.MEDIUM]: "p3",
  [Priority.LOW]: "p4",
  [Priority.LOWEST]: "p5",
};

/** A priority's colour (the row's edge); undefined for none. */
export function priorityColor(t: Theme, priority: Priority): string | undefined {
  const key = priorityColors[priority];
  return key === undefined ? undefined : t[key];
}
