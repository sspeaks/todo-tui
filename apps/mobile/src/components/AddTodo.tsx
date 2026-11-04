// Adding an item, on its own sheet opened by the + button: the title at
// the top (where the keyboard cannot cover it), the preview of what will
// be captured, then one row per field of one-tap choices (due, priority,
// status, repeat, labels, waiting on), each wrapping so every choice is in
// sight. Tapping a chosen
// pill again clears it. Only a calendar date, a new label, a new name or
// an unusual repeat opens a sheet. The choices become capture tokens after
// the title (capture-draft.ts), so it is one capture with ParseCapture's
// live preview and one Undo, and typed tokens still work. The request id
// is kept until the capture succeeds or the text changes, so a retry after
// an unknown outcome cannot capture twice.
import { type CaptureDraft, Code, ConnectError, CreatedVia, captureItem, isUnreachable, parseCapture, ulid } from "@todo-tui/client/portable";
import { useClients, useLiveStore } from "@todo-tui/client/react";
import { Latest, captureSummary, describeRule } from "@todo-tui/present";
import { router } from "expo-router";
import { useMemo, useRef, useState } from "react";
import { ActivityIndicator, KeyboardAvoidingView, Modal, Platform, Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";
import { type CaptureChoices, NO_CHOICES, captureText, hasChoices } from "../capture-draft";
import { useKnown } from "../known";
import { useOutbox } from "../outbox";
import { success } from "../haptics";
import { useSnack } from "../snack";
import { type Theme, useTheme } from "../theme";
import { Pill, PillGroup } from "./quick/Pill";
import { REPEAT_PRESETS, type RepeatPreset } from "./quick/presets";
import { QuickDate } from "./quick/QuickDate";
import { QuickLabels } from "./quick/QuickLabels";
import { QuickPriority } from "./quick/QuickPriority";
import { QuickRepeat } from "./quick/QuickRepeat";
import { QuickWaiting } from "./quick/QuickWaiting";

export function AddTodo({ visible, today, onClose }: { visible: boolean; today: number; onClose: () => void }) {
  const t = useTheme();
  const s = styles(t);
  const clients = useClients();
  const snack = useSnack();
  const store = useLiveStore();
  const outbox = useOutbox();
  const known = useKnown();
  const [title, setTitle] = useState("");
  const [choices, setChoices] = useState<CaptureChoices>(NO_CHOICES);
  /** How a repeat from the sheet reads ("every Mon, Thu"); presets need none. */
  const [customRepeat, setCustomRepeat] = useState<string>();
  const [preview, setPreview] = useState<{ text: string; draft: CaptureDraft }>();
  const [busy, setBusy] = useState(false);
  const requestId = useRef<string | undefined>(undefined);

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

  /** Any change to what would be captured: a new request id, a new preview. */
  const update = (nextTitle: string, next: CaptureChoices) => {
    setTitle(nextTitle);
    setChoices(next);
    requestId.current = undefined;
    if (nextTitle.trim()) previews.request(captureText(nextTitle, next));
  };
  const choose = (patch: Partial<CaptureChoices>) => update(title, { ...choices, ...patch });

  const reset = () => {
    setTitle("");
    setChoices(NO_CHOICES);
    setCustomRepeat(undefined);
    setPreview(undefined);
    requestId.current = undefined;
  };
  /** Cancel keeps nothing: the next + starts afresh. */
  const cancel = () => {
    reset();
    onClose();
  };

  const text = captureText(title, choices);
  /** Kept on the phone to send later: the computer is out of reach. */
  const keep = async (id: string) => {
    await outbox.add({ requestId: id, text, title: title.trim(), at: new Date().toISOString() });
    success();
    snack.show(`Saved on this phone: ${title.trim()}. It'll be added when your computer is back.`);
    reset();
    onClose();
  };

  const submit = async () => {
    if (!title.trim() || busy) return;
    const id = (requestId.current ??= ulid());
    // Not connected: don't wait for a request that cannot get through.
    if (store.getConnection().state !== "live") return keep(id);
    setBusy(true);
    try {
      const res = await captureItem(clients.items, text, { createdVia: CreatedVia.MOBILE, requestId: requestId.current, timeoutMs: 15_000 });
      const added = res.item?.id;
      success();
      snack.done(
        `Added: ${res.item?.title ?? title}`,
        { ...(res.result && { result: res.result }), items: [res.item] },
        added ? [{ label: "Details", onPress: () => router.push({ pathname: "/item/[id]", params: { id: added } }) }] : [],
      );
      reset();
      onClose();
    } catch (err) {
      if (isUnreachable(err) || (err instanceof ConnectError && err.code === Code.DeadlineExceeded)) await keep(id);
      else snack.fail(err);
    } finally {
      setBusy(false);
    }
  };

  const draft = preview?.text === text && title.trim() ? preview.draft : undefined;
  const warnings = draft?.tokens.filter((tok) => tok.warning).map((tok) => tok.warning) ?? [];
  const showPreview = draft && (draft.tokens.length > 0 || hasChoices(choices));
  const preset = REPEAT_PRESETS.find((p) => p.text === choices.repeat)?.text;

  const canAdd = title.trim().length > 0 && !busy;
  return (
    <Modal visible={visible} animationType="slide" presentationStyle="pageSheet" onRequestClose={cancel}>
      <KeyboardAvoidingView style={s.page} behavior={Platform.OS === "ios" ? "padding" : undefined}>
        <View style={s.header}>
          <Pressable onPress={cancel} hitSlop={8}>
            <Text style={s.cancel}>Cancel</Text>
          </Pressable>
          <Text style={s.heading}>New todo</Text>
          <Pressable
            onPress={submit}
            disabled={!canAdd}
            hitSlop={8}
            accessibilityLabel="Add"
            style={({ pressed }) => [s.add, !canAdd && s.disabled, pressed && s.pressed]}
          >
            {busy ? <ActivityIndicator color={t.onAccent} /> : <Text style={s.addText}>Add</Text>}
          </Pressable>
        </View>
        <ScrollView style={s.page} contentContainerStyle={s.content} keyboardShouldPersistTaps="always">
          <TextInput
            style={s.input}
            value={title}
            onChangeText={(next) => update(next, choices)}
            placeholder="What needs doing?"
            placeholderTextColor={t.dim}
            returnKeyType="done"
            onSubmitEditing={submit}
            submitBehavior="submit"
            autoFocus
            multiline
            accessibilityLabel="Title"
          />
          {showPreview ? <Text style={s.preview}>{captureSummary(draft, today)}</Text> : null}
          {warnings.map((w) => (
            <Text key={w} style={s.warn}>
              {w}
            </Text>
          ))}
          <View style={s.choices}>
            <PillGroup label="Due">
              <QuickDate title="Due" value={choices.due} today={today} onChange={(due) => choose({ due })} />
            </PillGroup>
            <PillGroup label="Priority">
              <QuickPriority value={choices.priority} onChange={(priority) => choose({ priority })} />
            </PillGroup>
            <PillGroup label="Status">
              <Pill label="Next" on={choices.status === "next"} onPress={() => choose({ status: choices.status === "next" ? undefined : "next" })} />
              <Pill
                label="Someday"
                on={choices.status === "someday"}
                onPress={() => choose({ status: choices.status === "someday" ? undefined : "someday" })}
              />
            </PillGroup>
            <PillGroup label="Repeat">
              <QuickRepeat
                preset={preset as RepeatPreset | undefined}
                custom={preset ? undefined : customRepeat}
                anchor={choices.due}
                current={choices.repeat}
                onPreset={(p) => {
                  setCustomRepeat(undefined);
                  choose({ repeat: p });
                }}
                onChange={(c) => {
                  setCustomRepeat(c ? describeRule(c.rule) : undefined);
                  choose({ repeat: c?.text });
                }}
              />
            </PillGroup>
            <PillGroup label="Labels">
              <QuickLabels value={choices.labels} known={known.labels} onChange={(labels) => choose({ labels })} />
            </PillGroup>
            <PillGroup label="Waiting on">
              <QuickWaiting value={choices.waiting} known={known.names} onChange={(waiting) => choose({ waiting })} />
            </PillGroup>
          </View>
        </ScrollView>
      </KeyboardAvoidingView>
    </Modal>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    header: {
      flexDirection: "row",
      alignItems: "center",
      justifyContent: "space-between",
      paddingHorizontal: 16,
      paddingVertical: 14,
      borderBottomWidth: StyleSheet.hairlineWidth,
      borderBottomColor: t.border,
    },
    heading: { color: t.text, fontWeight: "600", fontSize: 16 },
    cancel: { color: t.dim, fontSize: 16 },
    add: { backgroundColor: t.accent, borderRadius: 20, minWidth: 76, paddingHorizontal: 20, paddingVertical: 10, alignItems: "center" },
    addText: { color: t.onAccent, fontWeight: "700", fontSize: 17 },
    disabled: { opacity: 0.4 },
    pressed: { opacity: 0.8 },
    content: { paddingVertical: 12, gap: 10, paddingBottom: 32 },
    input: { color: t.text, fontSize: 20, paddingHorizontal: 16, paddingVertical: 6, minHeight: 44 },
    preview: { color: t.dim, fontSize: 14, lineHeight: 20, paddingHorizontal: 16 },
    warn: { color: t.warn, fontSize: 13, paddingHorizontal: 16 },
    choices: { gap: 10, marginTop: 6 },
  });
