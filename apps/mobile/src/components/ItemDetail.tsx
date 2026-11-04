// One Item, every field the phone edits: title, status, priority, due,
// repeat, labels, waiting (and its follow-up), hide until, notes; and
// delete. Each change is its own op, applied at once and undoable from the
// snackbar.
import {
  type Item,
  type MessageInitShape,
  type ItemSchema,
  Priority,
  Status,
  deleteItem,
  setRecurrence,
  setStatus,
  updateItem,
} from "@todo-tui/client/portable";
import { useClients, useItem } from "@todo-tui/client/react";
import { createdViaName, dayLabel, dayOf, describeRule, localTime, personName, priorityName, repeatsLine, ruleText, shortId } from "@todo-tui/present";
import { Stack, router } from "expo-router";
import { type ReactNode, useState } from "react";
import { ActivityIndicator, Alert, KeyboardAvoidingView, Platform, Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";
import { protoDate } from "../dates";
import { useKnown } from "../known";
import { useSnack } from "../snack";
import { type Theme, useTheme } from "../theme";
import { useToday } from "../today";
import { DateSheet } from "./pickers/DateSheet";
import { LabelsSheet } from "./pickers/LabelsSheet";
import { PrioritySheet } from "./pickers/PrioritySheet";
import { RepeatSheet } from "./pickers/RepeatSheet";
import { ALL_STATUSES, StatusPicker } from "./pickers/StatusPicker";
import { WaitingSheet } from "./pickers/WaitingSheet";

type Open = "priority" | "due" | "repeat" | "labels" | "waiting" | "followUp" | "defer";
type Patch = Omit<MessageInitShape<typeof ItemSchema>, "id" | "$typeName">;
// The plain-object form of Waiting (not the message, which needs $typeName).
type WaitingInit = Extract<NonNullable<Patch["waiting"]>, { $typeName?: undefined }>;

const closed = (s: Status) => s === Status.DONE || s === Status.CANCELLED;

export function ItemDetail({ id }: { id: string }) {
  const t = useTheme();
  const s = styles(t);
  const item = useItem(id, { fetch: true });
  if (!item)
    return (
      <View style={s.center}>
        <ActivityIndicator color={t.accent} />
      </View>
    );
  return <Loaded item={item} />;
}

function Loaded({ item }: { item: Item }) {
  const t = useTheme();
  const s = styles(t);
  const clients = useClients();
  const snack = useSnack();
  const today = useToday();
  const known = useKnown();
  const [open, setOpen] = useState<Open>();
  /** The title and notes while being edited; undefined shows the Item's. */
  const [title, setTitle] = useState<string>();
  const [notes, setNotes] = useState<string>();
  const close = () => setOpen(undefined);

  /** The Item's waiting with `change` applied, as an init: its fields copied, not the message spread. */
  const waitingWith = (change: Partial<WaitingInit>): WaitingInit => {
    const w = item.waiting;
    const base: WaitingInit = {
      ...(w?.on && { on: w.on }),
      ...(w?.reason && { reason: w.reason }),
      ...(w?.since && { since: w.since }),
      ...(w?.followUpOn && { followUpOn: w.followUpOn }),
    };
    return { ...base, ...change };
  };

  const update = async (patch: Patch, paths: string[], text: string, opts: { expectRevision?: boolean } = {}) => {
    try {
      const res = await updateItem(clients.items, { id: item.id, ...patch }, paths, {
        timeoutMs: 15_000,
        ...(opts.expectRevision && { expectedRevision: item.revision }),
      });
      snack.done(text, { ...(res.result && { result: res.result }), items: [res.item] });
      return true;
    } catch (err) {
      snack.fail(err);
      return false;
    }
  };

  const saveTitle = async () => {
    const next = title?.trim();
    if (next === undefined || next === item.title || !next) return setTitle(undefined);
    if (await update({ title: next }, ["title"], "Title changed", { expectRevision: true })) setTitle(undefined);
  };
  const saveNotes = async () => {
    if (notes === undefined || notes === item.description) return setNotes(undefined);
    if (await update({ description: notes }, ["description"], "Notes saved", { expectRevision: true })) setNotes(undefined);
  };

  const changeStatus = async (status: Status) => {
    if (status === item.status) return;
    try {
      const res = await setStatus(clients.items, item.id, status, { timeoutMs: 15_000 });
      const label = ALL_STATUSES.find((x) => x.value === status)?.label ?? "";
      const next = res.generated?.dueDate ? ` · next ${dayLabel(dayOf(res.generated.dueDate), today)}` : "";
      snack.done(`${label}${next}`, { ...(res.result && { result: res.result }), items: [res.item, res.generated] });
    } catch (err) {
      snack.fail(err);
    }
  };

  const changeRepeat = async (choice: { rule: Parameters<typeof setRecurrence>[2] } | undefined) => {
    try {
      const res = await setRecurrence(clients.items, item.id, choice?.rule, { timeoutMs: 15_000 });
      snack.done(choice ? "Repeat set" : "Repeat stopped", { ...(res.result && { result: res.result }), items: [res.item] });
    } catch (err) {
      snack.fail(err);
    }
  };

  const confirmDelete = () =>
    Alert.alert("Delete this item?", item.subtasksTotal ? "Its subtasks are deleted with it. Undo brings them back." : "Undo brings it back.", [
      { text: "Cancel", style: "cancel" },
      {
        text: "Delete",
        style: "destructive",
        onPress: async () => {
          try {
            const res = await deleteItem(clients.items, item.id, { timeoutMs: 15_000 });
            snack.done(`Deleted: ${item.title}`, { ...(res.result && { result: res.result }), items: [res.item] });
            router.back();
          } catch (err) {
            snack.fail(err);
          }
        },
      },
    ]);

  const due = item.dueDate ? dayOf(item.dueDate) : undefined;
  const defer = item.deferUntil ? dayOf(item.deferUntil) : undefined;
  const waitingOn = personName(item.waiting?.on);
  const followUp = item.waiting?.followUpOn ? dayOf(item.waiting.followUpOn) : undefined;
  const rule = item.recurrence?.rule;
  const isClosed = closed(item.status);
  const notesDirty = notes !== undefined && notes !== item.description;

  return (
    <KeyboardAvoidingView style={s.page} behavior={Platform.OS === "ios" ? "padding" : undefined} keyboardVerticalOffset={100}>
      <Stack.Screen options={{ title: shortId(item.id) }} />
      <ScrollView contentContainerStyle={s.content} keyboardShouldPersistTaps="handled">
        <TextInput
          style={[s.title, isClosed && s.closedTitle]}
          value={title ?? item.title}
          onFocus={() => setTitle(item.title)}
          onChangeText={setTitle}
          onEndEditing={saveTitle}
          onSubmitEditing={saveTitle}
          submitBehavior="blurAndSubmit"
          returnKeyType="done"
          multiline
          accessibilityLabel="Title"
        />

        <StatusPicker value={item.status} options={ALL_STATUSES} onChange={changeStatus} />

        <View style={s.card}>
          <Field label="Priority" value={item.priority === Priority.NONE ? undefined : priorityName(item.priority)} onPress={() => setOpen("priority")} />
          <Field label="Due" value={due === undefined ? undefined : dayLabel(due, today)} danger={item.isOverdue} onPress={() => setOpen("due")} />
          <Field
            label="Repeats"
            value={repeatsLine(item, today) ?? (rule ? describeRule(rule) : undefined)}
            hint={isClosed && !rule ? "Reopen it to set a repeat" : undefined}
            onPress={isClosed ? undefined : () => setOpen("repeat")}
          />
          <Field label="Labels" value={item.labels.length ? item.labels.map((l) => `#${l}`).join(" ") : undefined} onPress={() => setOpen("labels")} />
          <Field label="Waiting on" value={waitingOn || undefined} onPress={() => setOpen("waiting")} />
          {item.waiting ? (
            <Field
              label="Follow up"
              value={followUp === undefined ? undefined : dayLabel(followUp, today)}
              danger={followUp !== undefined && followUp <= today}
              onPress={() => setOpen("followUp")}
            />
          ) : null}
          <Field label="Hide until" value={defer === undefined ? undefined : dayLabel(defer, today)} onPress={() => setOpen("defer")} last />
        </View>

        <Text style={s.section}>Notes</Text>
        <TextInput
          style={s.notes}
          value={notes ?? item.description}
          onFocus={() => setNotes(item.description)}
          onChangeText={setNotes}
          placeholder="Add notes"
          placeholderTextColor={t.dim}
          multiline
          textAlignVertical="top"
          accessibilityLabel="Notes"
        />
        {notesDirty ? (
          <View style={s.notesActions}>
            <Pressable onPress={() => setNotes(undefined)} hitSlop={8}>
              <Text style={s.cancel}>Discard</Text>
            </Pressable>
            <Pressable style={s.save} onPress={saveNotes}>
              <Text style={s.saveText}>Save notes</Text>
            </Pressable>
          </View>
        ) : null}

        <View style={s.info}>
          {item.parentTitle && item.parentId ? (
            <Pressable onPress={() => router.push({ pathname: "/item/[id]", params: { id: item.parentId! } })}>
              <Text style={s.infoText}>
                Part of <Text style={s.link}>{item.parentTitle}</Text>
              </Text>
            </Pressable>
          ) : null}
          {item.subtasksTotal ? (
            <Text style={s.infoText}>
              Subtasks: {item.subtasksDone} of {item.subtasksTotal} done
            </Text>
          ) : null}
          {item.createdAt ? (
            <Text style={s.infoText}>
              Created {localTime(item.createdAt)}
              {createdViaName(item.createdVia) ? ` from ${createdViaName(item.createdVia)}` : ""}
            </Text>
          ) : null}
          {item.updatedAt ? <Text style={s.infoText}>Changed {localTime(item.updatedAt)}</Text> : null}
        </View>

        <Pressable style={s.delete} onPress={confirmDelete}>
          <Text style={s.deleteText}>Delete</Text>
        </Pressable>
      </ScrollView>

      {open === "priority" ? (
        <PrioritySheet
          visible
          value={item.priority}
          onChange={(p) => void update(p === undefined ? {} : { priority: p }, ["priority"], p === undefined ? "Priority cleared" : `Priority ${priorityName(p)}`)}
          onClose={close}
        />
      ) : null}
      {open === "due" ? (
        <DateSheet
          title="Due"
          visible
          value={due}
          today={today}
          onChange={(d) => void update(d === undefined ? {} : { dueDate: protoDate(d) }, ["due_date"], d === undefined ? "Due date cleared" : `Due ${dayLabel(d, today)}`)}
          onClose={close}
        />
      ) : null}
      {open === "defer" ? (
        <DateSheet
          title="Hide until"
          visible
          value={defer}
          today={today}
          onChange={(d) =>
            void update(d === undefined ? {} : { deferUntil: protoDate(d) }, ["defer_until"], d === undefined ? "Shown again" : `Hidden until ${dayLabel(d, today)}`)
          }
          onClose={close}
        />
      ) : null}
      {open === "followUp" ? (
        <DateSheet
          title="Follow up"
          visible
          value={followUp}
          today={today}
          onChange={(d) =>
            void update(
              {
                waiting:
                  d === undefined
                    ? (({ followUpOn: _, ...rest }) => rest)(waitingWith({}))
                    : waitingWith({ followUpOn: protoDate(d) }),
              },
              ["waiting.follow_up_on"],
              d === undefined ? "Follow-up cleared" : `Follow up ${dayLabel(d, today)}`,
            )
          }
          onClose={close}
        />
      ) : null}
      {open === "repeat" ? (
        <RepeatSheet visible value={rule ? ruleText(rule) : undefined} anchor={due} onChange={(c) => void changeRepeat(c)} onClose={close} />
      ) : null}
      {open === "labels" ? (
        <LabelsSheet
          visible
          value={item.labels}
          known={known.labels}
          onChange={(labels) => void update({ labels }, ["labels"], labels.length ? `Labels ${labels.map((l) => `#${l}`).join(" ")}` : "Labels cleared")}
          onClose={close}
        />
      ) : null}
      {open === "waiting" ? (
        <WaitingSheet
          visible
          value={waitingOn || undefined}
          known={known.names}
          onChange={(name) =>
            void update(
              name === undefined ? {} : { waiting: waitingWith({ on: { ref: { case: "freeText", value: name } } }) },
              ["waiting"],
              name === undefined ? "No longer waiting" : `Waiting on ${name}`,
            )
          }
          onClose={close}
        />
      ) : null}
    </KeyboardAvoidingView>
  );
}

function Field({
  label,
  value,
  hint,
  danger,
  onPress,
  last,
}: {
  label: string;
  value: string | undefined;
  hint?: string | undefined;
  danger?: boolean;
  onPress: (() => void) | undefined;
  last?: boolean;
}): ReactNode {
  const s = styles(useTheme());
  return (
    <Pressable style={[s.field, !last && s.fieldSep]} onPress={onPress} disabled={!onPress} accessibilityRole="button" accessibilityLabel={label}>
      <Text style={s.fieldLabel}>{label}</Text>
      <Text style={[s.fieldValue, value === undefined && s.fieldEmpty, danger && s.danger]} numberOfLines={2}>
        {value ?? hint ?? (onPress ? "Add" : "—")}
      </Text>
    </Pressable>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    page: { flex: 1, backgroundColor: t.bg },
    center: { flex: 1, alignItems: "center", justifyContent: "center", backgroundColor: t.bg },
    content: { padding: 16, gap: 14, paddingBottom: 48 },
    title: { color: t.text, fontSize: 22, fontWeight: "600", padding: 0 },
    closedTitle: { color: t.dim, textDecorationLine: "line-through" },
    card: { backgroundColor: t.surface, borderRadius: 12, overflow: "hidden" },
    field: { flexDirection: "row", justifyContent: "space-between", alignItems: "center", paddingHorizontal: 14, paddingVertical: 13, gap: 12 },
    fieldSep: { borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: t.border },
    fieldLabel: { color: t.text, fontSize: 15 },
    fieldValue: { color: t.text, fontSize: 15, flexShrink: 1, textAlign: "right" },
    fieldEmpty: { color: t.dim },
    danger: { color: t.danger, fontWeight: "600" },
    section: { color: t.dim, fontSize: 13, textTransform: "uppercase", marginTop: 4 },
    notes: { backgroundColor: t.surface, color: t.text, borderRadius: 12, padding: 12, minHeight: 120, fontSize: 15 },
    notesActions: { flexDirection: "row", justifyContent: "flex-end", alignItems: "center", gap: 16 },
    cancel: { color: t.dim, fontSize: 15 },
    save: { backgroundColor: t.accent, borderRadius: 8, paddingHorizontal: 14, paddingVertical: 8 },
    saveText: { color: t.onAccent, fontWeight: "600" },
    info: { gap: 4, marginTop: 4 },
    infoText: { color: t.dim, fontSize: 13 },
    link: { color: t.accent },
    delete: { alignSelf: "center", marginTop: 16, paddingHorizontal: 16, paddingVertical: 10 },
    deleteText: { color: t.danger, fontSize: 16 },
  });
