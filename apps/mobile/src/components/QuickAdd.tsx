// Quick-add with the TUI's live preview (Decision 7): every keystroke asks
// ParseCapture, the latest text winning, and the summary shows what would
// be captured. Submitting keeps the request id until it succeeds or the
// text changes, so a retry after an unknown outcome cannot capture twice.
import { type CaptureDraft, CreatedVia, captureItem, parseCapture, ulid } from "@todo-tui/client/portable";
import { useClients, useLiveStore } from "@todo-tui/client/react";
import { Latest, captureSummary } from "@todo-tui/present";
import { useMemo, useRef, useState } from "react";
import { ActivityIndicator, Pressable, StyleSheet, Text, TextInput, View } from "react-native";
import { describeError } from "../errors";
import { type Theme, useTheme } from "../theme";

export function QuickAdd({ today, onCaptured }: { today: number; onCaptured: (title: string, opId: string | undefined) => void }) {
  const t = useTheme();
  const s = styles(t);
  const clients = useClients();
  const store = useLiveStore();
  const [text, setText] = useState("");
  const [preview, setPreview] = useState<{ text: string; draft: CaptureDraft }>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const requestId = useRef<string | undefined>(undefined);

  // A preview for older text is kept but not shown: render compares texts.
  const previews = useMemo(
    () =>
      new Latest<string, CaptureDraft | undefined>(
        async (input) => (await parseCapture(clients.items, input, { timeoutMs: 5_000 })).draft,
        (input, draft) => {
          if (draft) setPreview({ text: input, draft });
        },
      ),
    [clients],
  );

  const onChange = (next: string) => {
    setText(next);
    requestId.current = undefined;
    setError(undefined);
    if (next.trim()) previews.request(next);
  };

  const submit = async () => {
    if (!text.trim() || busy) return;
    requestId.current ??= ulid();
    setBusy(true);
    try {
      const res = await captureItem(clients.items, text, { createdVia: CreatedVia.MOBILE, requestId: requestId.current, timeoutMs: 15_000 });
      if (res.item && res.result) store.applyItems([res.item], res.result.revision);
      onCaptured(res.item?.title ?? text, res.result?.opId);
      setText("");
      setPreview(undefined);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  };

  const draft = preview?.text === text && text.trim() ? preview.draft : undefined;
  const warnings = draft?.tokens.filter((tok) => tok.warning).map((tok) => tok.warning) ?? [];
  const hasTokens = (draft?.tokens.length ?? 0) > 0;

  return (
    <View style={s.wrap}>
      {draft && hasTokens ? (
        <Text style={s.preview} numberOfLines={2}>
          {captureSummary(draft, today)}
        </Text>
      ) : null}
      {warnings.map((w) => (
        <Text key={w} style={s.warn}>
          {w}
        </Text>
      ))}
      {error ? <Text style={s.error}>{error}</Text> : null}
      <View style={s.bar}>
        <TextInput
          style={s.input}
          value={text}
          onChangeText={onChange}
          placeholder="call dentist ^fri +2 #home"
          placeholderTextColor={t.dim}
          returnKeyType="done"
          onSubmitEditing={submit}
          submitBehavior="submit"
          autoCorrect={false}
        />
        <Pressable style={[s.add, (!text.trim() || busy) && s.disabled]} onPress={submit} disabled={!text.trim() || busy} accessibilityLabel="Add">
          {busy ? <ActivityIndicator color={t.onAccent} /> : <Text style={s.addText}>Add</Text>}
        </Pressable>
      </View>
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    wrap: { borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: t.border, padding: 8, gap: 4, backgroundColor: t.bg },
    preview: { color: t.dim, fontSize: 13, paddingHorizontal: 4 },
    warn: { color: t.warn, fontSize: 13, paddingHorizontal: 4 },
    error: { color: t.danger, fontSize: 13, paddingHorizontal: 4 },
    bar: { flexDirection: "row", gap: 8 },
    input: { flex: 1, backgroundColor: t.surface, color: t.text, borderRadius: 8, paddingHorizontal: 12, paddingVertical: 10, fontSize: 16 },
    add: { backgroundColor: t.accent, borderRadius: 8, paddingHorizontal: 16, justifyContent: "center" },
    addText: { color: t.onAccent, fontWeight: "600" },
    disabled: { opacity: 0.5 },
  });
