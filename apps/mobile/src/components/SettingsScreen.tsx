// The computer this phone is paired with and how the link is, what waits
// to be sent, and pairing: with another computer, or none.
import { Ionicons } from "@expo/vector-icons";
import { useConnection } from "@todo-tui/client/react";
import { router } from "expo-router";
import type { ReactNode } from "react";
import { Alert, Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { useOutbox } from "../outbox";
import { APP_VERSION, useSession } from "../session";
import { type Theme, useTheme } from "../theme";

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
        <Row icon="desktop-outline" label={host} />
        <Row icon="pulse-outline" label="Status" value={status.text} valueColor={status.color} />
        {link.state === "ok" ? <Row icon="information-circle-outline" label="todo on it" value={`${link.serverInfo.daemonVersion} · API ${link.serverInfo.apiMinor}`} /> : null}
        {link.state !== "ok" || conn.state !== "live" ? <Row icon="refresh" label="Reconnect now" onPress={retry} accent /> : null}
      </Group>

      {pending.length > 0 ? (
        <Group title="Added offline">
          {pending.map((p) => (
            <Row key={p.requestId} icon="cloud-upload-outline" label={p.title} value="waiting" />
          ))}
        </Group>
      ) : null}

      <Group title="Pairing">
        <Row icon="swap-horizontal" label="Pair with a different computer" onPress={() => router.push("/pair")} accent />
        <Row icon="log-out-outline" label="Unpair this phone" onPress={confirmUnpair} danger />
      </Group>

      <Text style={s.footer}>todo for iPhone {APP_VERSION}</Text>
    </ScrollView>
  );
}

function Group({ title, children }: { title: string; children: ReactNode }) {
  const s = styles(useTheme());
  return (
    <View style={s.group}>
      <Text style={s.groupTitle}>{title}</Text>
      <View style={s.card}>{children}</View>
    </View>
  );
}

function Row({
  icon,
  label,
  value,
  valueColor,
  onPress,
  accent,
  danger,
}: {
  icon: keyof typeof Ionicons.glyphMap;
  label: string;
  value?: string;
  valueColor?: string;
  onPress?: () => void;
  accent?: boolean;
  danger?: boolean;
}) {
  const t = useTheme();
  const s = styles(t);
  const color = danger ? t.danger : accent ? t.accent : t.text;
  return (
    <Pressable style={({ pressed }) => [s.row, pressed && onPress && s.pressed]} onPress={onPress} disabled={!onPress} accessibilityRole={onPress ? "button" : "text"}>
      <Ionicons name={icon} size={20} color={danger ? t.danger : accent ? t.accent : t.dim} />
      <Text style={[s.label, { color }]} numberOfLines={2}>
        {label}
      </Text>
      {value ? (
        <Text style={[s.value, valueColor ? { color: valueColor } : null]} numberOfLines={3}>
          {value}
        </Text>
      ) : null}
    </Pressable>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    content: { padding: 16, gap: 24, paddingBottom: 48 },
    group: { gap: 8 },
    groupTitle: { color: t.dim, fontSize: 13, textTransform: "uppercase", paddingHorizontal: 4 },
    card: { backgroundColor: t.surface, borderRadius: 12, overflow: "hidden" },
    row: {
      flexDirection: "row",
      alignItems: "center",
      gap: 12,
      paddingHorizontal: 14,
      paddingVertical: 13,
      borderBottomWidth: StyleSheet.hairlineWidth,
      borderBottomColor: t.border,
    },
    pressed: { opacity: 0.6 },
    label: { flex: 1, fontSize: 15 },
    value: { flexShrink: 1, maxWidth: "55%", color: t.dim, fontSize: 14, textAlign: "right" },
    footer: { color: t.dim, fontSize: 12, textAlign: "center" },
  });
