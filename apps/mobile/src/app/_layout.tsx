import "../polyfills";
import { JetBrainsMono_400Regular } from "@expo-google-fonts/jetbrains-mono/400Regular";
import { JetBrainsMono_700Bold } from "@expo-google-fonts/jetbrains-mono/700Bold";
import { ClientsProvider, LiveStoreProvider } from "@todo-tui/client/react";
import { useFonts } from "expo-font";
import { Stack } from "expo-router";
import * as SplashScreen from "expo-splash-screen";
import { StatusBar } from "expo-status-bar";
import { type ReactNode, useEffect, useState } from "react";
import { SafeAreaProvider } from "react-native-safe-area-context";
import { CacheWriter } from "../cache";
import { OutboxProvider } from "../outbox";
import { SessionProvider, useSession } from "../session";
import { SnackProvider } from "../snack";
import { fallBackToSystemMono, useTheme } from "../theme";

// The splash stays up until the mono font is in and the session has read
// the pairing and the kept list, so the list shows with no spinner before
// it (in global scope, as the splash screen docs ask: from a component it
// can be too late).
void SplashScreen.preventAutoHideAsync().catch(() => {});

/** How long the splash waits for the font (and the session) before the system's mono is used instead. */
const FONT_WAIT_MS = 3_000;

export default function RootLayout() {
  const [loaded, error] = useFonts({ JetBrainsMono_400Regular, JetBrainsMono_700Bold });
  const [gaveUp, setGaveUp] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setGaveUp(true), FONT_WAIT_MS);
    return () => clearTimeout(timer);
  }, []);
  // Before anything renders with it: a font that failed (or is slow) is not half used.
  const fallBack = !loaded && (error !== null || gaveUp);
  if (fallBack) fallBackToSystemMono();
  const ready = loaded || fallBack;
  // The session reads the keychain and the kept list while the font loads; it renders no text.
  return (
    <SessionProvider>
      <HideSplash ready={ready} gaveUp={gaveUp} />
      {ready ? <Screens /> : null}
    </SessionProvider>
  );
}

/** Hides the splash once the font is ready and the session knows what to show (or it has waited FONT_WAIT_MS). */
function HideSplash({ ready, gaveUp }: { ready: boolean; gaveUp: boolean }) {
  const { session } = useSession();
  const hide = ready && (session.state !== "loading" || gaveUp);
  useEffect(() => {
    if (hide) SplashScreen.hide();
  }, [hide]);
  return null;
}

function Screens() {
  const t = useTheme();
  return (
    <SafeAreaProvider>
      <StatusBar style="auto" />
      <Connected>
        <Stack
          screenOptions={{
            headerStyle: { backgroundColor: t.bg },
            headerTintColor: t.text,
            headerTitleStyle: t.monoBold,
            headerShadowVisible: false,
            contentStyle: { backgroundColor: t.bg },
          }}
        >
          <Stack.Screen name="(tabs)" options={{ headerShown: false, title: "todo" }} />
          <Stack.Screen name="pair" options={{ title: "Pair with your computer" }} />
          <Stack.Screen name="item/[id]" options={{ title: "" }} />
        </Stack>
      </Connected>
    </SafeAreaProvider>
  );
}

/**
 * While connected, every screen gets the store, the RPC clients and the
 * snackbar, and the list is kept for the next launch. Coming back from the
 * background swaps in a new store with the session still ready, so the
 * screens stay mounted.
 */
function Connected({ children }: { children: ReactNode }) {
  const { session } = useSession();
  if (session.state !== "ready") return children;
  return (
    <LiveStoreProvider store={session.store}>
      <CacheWriter store={session.store} url={session.pairing.url} />
      <ClientsProvider clients={session.conn}>
        <SnackProvider>
          <OutboxProvider url={session.pairing.url}>{children}</OutboxProvider>
        </SnackProvider>
      </ClientsProvider>
    </LiveStoreProvider>
  );
}
