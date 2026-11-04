// Pairing: scan the QR code `todo remote pair` prints, open its
// todo-tui://pair link (which routes here with url and token), or paste
// the server and token.
import { parsePairingLink } from "@todo-tui/client/portable";
import { type BarcodeScanningResult, CameraView, useCameraPermissions } from "expo-camera";
import { router, useLocalSearchParams } from "expo-router";
import { useCallback, useRef, useState } from "react";
import { ActivityIndicator, Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";
import { useSession } from "../session";
import { type Theme, useTheme } from "../theme";

export default function Pair() {
  const t = useTheme();
  const s = styles(t);
  const { session, pair } = useSession();
  const params = useLocalSearchParams<{ url?: string; token?: string }>();
  const [url, setUrl] = useState(params.url ?? "");
  const [token, setToken] = useState(params.token ?? "");
  const [scanning, setScanning] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [permission, requestPermission] = useCameraPermissions();
  const scanned = useRef(false);

  const submit = useCallback(
    async (p: { url: string; token: string }) => {
      setBusy(true);
      setError(undefined);
      try {
        await pair({ url: p.url.trim().replace(/\/+$/, ""), token: p.token.trim() });
        router.replace("/");
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setBusy(false);
      }
    },
    [pair],
  );

  const onScanned = (result: BarcodeScanningResult) => {
    if (scanned.current) return;
    const p = parsePairingLink(result.data);
    if (!p) {
      setError("That QR code isn't a todo pairing code. Run `todo remote pair` on your computer.");
      return;
    }
    scanned.current = true;
    setScanning(false);
    setUrl(p.url);
    setToken(p.token);
    void submit(p);
  };

  const startScan = async () => {
    setError(undefined);
    scanned.current = false;
    if (!permission?.granted) {
      const res = await requestPermission();
      if (!res.granted) {
        setError("Camera access is off. Allow it in Settings, or paste the server and token below.");
        return;
      }
    }
    setScanning(true);
  };

  const notice = session.state === "unpaired" ? session.message : undefined;
  // A pairing link only fills the fields: connecting to a server is the
  // person's choice, not a link's.
  const fromLink = Boolean(params.url && params.token);

  return (
    <ScrollView contentContainerStyle={s.page} keyboardShouldPersistTaps="handled">
      {notice ? <Text style={s.warn}>{notice}</Text> : null}
      {fromLink ? <Text style={s.text}>Check that the server below is your computer, then tap Connect.</Text> : null}
      <Text style={s.text}>
        On your computer, run <Text style={s.code}>todo remote pair --name phone</Text> and scan the code it prints.
      </Text>

      {scanning ? (
        <View style={s.camera}>
          <CameraView style={StyleSheet.absoluteFill} barcodeScannerSettings={{ barcodeTypes: ["qr"] }} onBarcodeScanned={onScanned} />
          <Pressable style={s.cancelScan} onPress={() => setScanning(false)}>
            <Text style={s.buttonText}>Cancel</Text>
          </Pressable>
        </View>
      ) : (
        <Pressable style={s.button} onPress={startScan} disabled={busy}>
          <Text style={s.buttonText}>Scan QR code</Text>
        </Pressable>
      )}

      <Text style={s.label}>Or enter them yourself</Text>
      <TextInput
        style={s.input}
        value={url}
        onChangeText={setUrl}
        placeholder="https://laptop.tailnet.ts.net"
        placeholderTextColor={t.dim}
        autoCapitalize="none"
        autoCorrect={false}
        keyboardType="url"
      />
      <TextInput
        style={s.input}
        value={token}
        onChangeText={setToken}
        placeholder="todo_…"
        placeholderTextColor={t.dim}
        autoCapitalize="none"
        autoCorrect={false}
        secureTextEntry
      />
      <Pressable style={[s.button, (!url || !token || busy) && s.disabled]} disabled={!url || !token || busy} onPress={() => submit({ url, token })}>
        {busy ? <ActivityIndicator color={t.onAccent} /> : <Text style={s.buttonText}>Connect</Text>}
      </Pressable>
      {error ? <Text style={s.error}>{error}</Text> : null}
    </ScrollView>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { padding: 16, gap: 12 },
    text: { color: t.text, fontSize: 15, lineHeight: 21 },
    code: { fontFamily: "Menlo", color: t.accent },
    label: { color: t.dim, marginTop: 12 },
    input: { borderWidth: 1, borderColor: t.border, borderRadius: 8, padding: 12, color: t.text, backgroundColor: t.surface, fontSize: 15 },
    button: { backgroundColor: t.accent, borderRadius: 8, padding: 14, alignItems: "center" },
    disabled: { opacity: 0.5 },
    buttonText: { color: t.onAccent, fontWeight: "600", fontSize: 15 },
    camera: { height: 320, borderRadius: 12, overflow: "hidden", justifyContent: "flex-end" },
    cancelScan: { margin: 12, alignSelf: "center", backgroundColor: "#0008", borderRadius: 8, paddingHorizontal: 16, paddingVertical: 8 },
    error: { color: t.danger },
    warn: { color: t.warn },
  });
