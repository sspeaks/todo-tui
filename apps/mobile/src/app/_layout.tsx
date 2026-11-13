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
import { OutboxProvider } from "../outbox";
import { SessionProvider, useSession } from "../session";
import { SnackProvider } from "../snack";
import { fallBackToSystemMono, useTheme } from "../theme";

// The splash stays up until the mono font is in (in global scope, as the
// splash screen docs ask: from a component it can be too late).
void SplashScreen.preventAutoHideAsync().catch(() => {});

/** How long the splash waits for the font before the system's mono is used instead. */
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
  useEffect(() => {
    if (ready) SplashScreen.hide();
  }, [ready]);
  if (!ready) return null;
  return <Screens />;
}

function Screens() {
  const t = useTheme();
  return (
    <SafeAreaProvider>
      <SessionProvider>
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
      </SessionProvider>
    </SafeAreaProvider>
  );
}

/**
 * While connected, every screen gets the store, the RPC clients and the
 * snackbar. Coming back from the background swaps in a new store with the
 * session still ready, so the screens stay mounted.
 */
function Connected({ children }: { children: ReactNode }) {
  const { session } = useSession();
  if (session.state !== "ready") return children;
  return (
    <LiveStoreProvider store={session.store}>
      <ClientsProvider clients={session.conn}>
        <SnackProvider>
          <OutboxProvider url={session.pairing.url}>{children}</OutboxProvider>
        </SnackProvider>
      </ClientsProvider>
    </LiveStoreProvider>
  );
}
