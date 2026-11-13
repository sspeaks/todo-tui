// The computer this phone is paired with and how the link is, what waits
// to be sent, and pairing: with another computer, or none.
import { useConnection } from "@todo-tui/client/react";
import { router } from "expo-router";
import { Alert, ScrollView, StyleSheet, Text } from "react-native";
import { useOutbox } from "../outbox";
import { APP_VERSION, useSession } from "../session";
import { size, space, type Theme, useTheme } from "../theme";
import { CardGroup as Group, CardRow } from "./Card";

export function SettingsScreen() {
  const t = useTheme();
  const s = styles(t);
  const { session, unpair, retry } = useSession();
  const conn = useConnection();
  const { pending } = useOutbox();
  if (session.state !== "ready") return null;
  const { link, pairing } = session;
  const host = pairing.url.replace(/^https?:\/\//, "");

  const status =
    link.state === "too-old"
      ? { text: link.message, color: t.danger }
      : link.state === "unreachable"
        ? { text: link.message, color: t.warn }
        : conn.state === "live"
          ? { text: "Connected", color: t.ok }
          : { text: "Connecting…", color: t.dim };

  const confirmUnpair = () =>
    Alert.alert(
      "Unpair this phone?",
      pending.length
        ? `${pending.length} item${pending.length === 1 ? "" : "s"} added offline will be sent if you pair with this computer again.`
        : "Your todos stay on your computer. To use the app again, pair with `todo remote pair`.",
      [
        { text: "Cancel", style: "cancel" },
        {
          text: "Unpair",
          style: "destructive",
          onPress: async () => {
            await unpair();
            router.replace("/pair");
          },
        },
      ],
    );

  return (
    <ScrollView style={s.page} contentContainerStyle={s.content}>
      <Group title="Computer">
        <CardRow icon="desktop-outline" label={host} />
        <CardRow icon="pulse-outline" label="Status" value={status.text} valueColor={status.color} />
        {link.state === "ok" ? <CardRow icon="information-circle-outline" label="todo on it" value={`${link.serverInfo.daemonVersion} · API ${link.serverInfo.apiMinor}`} /> : null}
        {link.state !== "ok" || conn.state !== "live" ? <CardRow icon="refresh" label="Reconnect now" onPress={retry} accent /> : null}
      </Group>

      {pending.length > 0 ? (
        <Group title="Added offline">
          {pending.map((p) => (
            <CardRow key={p.requestId} icon="cloud-upload-outline" label={p.title} value="waiting" />
          ))}
        </Group>
      ) : null}

      <Group title="Pairing">
        <CardRow icon="swap-horizontal" label="Pair with a different computer" onPress={() => router.push("/pair")} accent />
        <CardRow icon="log-out-outline" label="Unpair this phone" onPress={confirmUnpair} danger />
      </Group>

      <Text style={s.footer}>todo for iPhone {APP_VERSION}</Text>
    </ScrollView>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.grouped },
    content: { padding: space.l, gap: space.xl, paddingBottom: 48 },
    footer: { ...t.mono, color: t.dim, fontSize: size.caption, textAlign: "center" },
  });
