import "../polyfills";
import { ClientsProvider, LiveStoreProvider } from "@todo-tui/client/react";
import { Stack } from "expo-router";
import { StatusBar } from "expo-status-bar";
import type { ReactNode } from "react";
import { SafeAreaProvider } from "react-native-safe-area-context";
import { OutboxProvider } from "../outbox";
import { SessionProvider, useSession } from "../session";
import { SnackProvider } from "../snack";
import { useTheme } from "../theme";

export default function RootLayout() {
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
