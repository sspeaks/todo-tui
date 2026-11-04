import { type ParseRecurrenceResponse, type RecurrenceRule, parseRecurrence } from "@todo-tui/client/portable";
import { useClients } from "@todo-tui/client/react";
import { Latest, describeRule, ruleText } from "@todo-tui/present";
import { useEffect, useMemo, useState } from "react";
import { Pressable, Text, TextInput, View } from "react-native";
import { protoDate } from "../../dates";
import { useTheme } from "../../theme";
import { Option, Sheet, sheetStyles } from "./Sheet";

const PRESETS: { label: string; rule: string }[] = [
  { label: "Every day", rule: "daily" },
  { label: "Every weekday", rule: "weekdays" },
  { label: "Every week", rule: "weekly" },
  { label: "Every month", rule: "monthly" },
];

const DAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

export interface RepeatChoice {
  /** As ParseRecurrence takes it: "daily", "mon,thu", "2w". */
  text: string;
  rule: RecurrenceRule;
}

/**
 * A repeat rule: presets, weekdays, "after done", or typed, checked with
 * ParseRecurrence as you go. `anchor` (the due day) gives weekly and
 * monthly their day.
 */
export function RepeatSheet({
  visible,
  value,
  anchor,
  onChange,
  onClose,
}: {
  visible: boolean;
  /** The rule now, as text. */
  value: string | undefined;
  anchor: number | undefined;
  onChange: (choice: RepeatChoice | undefined) => void;
  onClose: () => void;
}) {
  const t = useTheme();
  const s = sheetStyles(t);
  const clients = useClients();
  const [text, setText] = useState(value ?? "");
  const [parsed, setParsed] = useState<{ text: string; res: ParseRecurrenceResponse }>();
  const [after, setAfter] = useState({ n: 3, unit: "d" as "d" | "w" });

  const latest = useMemo(
    () =>
      new Latest<string, ParseRecurrenceResponse>(
        (input) => parseRecurrence(clients.items, input, { timeoutMs: 5_000, ...(anchor !== undefined && { anchor: protoDate(anchor) }) }),
        (input, res) => setParsed({ text: input, res }),
      ),
    [clients, anchor],
  );
  useEffect(() => {
    if (text.trim()) latest.request(text.trim());
  }, [text, latest]);

  const current = parsed?.text === text.trim() ? parsed.res : undefined;
  const days = text.split(",").map((d) => d.trim().toLowerCase());
  const toggleDay = (d: string) => {
    const chosen = DAYS.filter((x) => (x === d ? !days.includes(x) : days.includes(x)));
    setText(chosen.join(","));
  };

  const done = () => {
    if (!text.trim()) return onClose();
    if (current?.rule) {
      onChange({ text: ruleText(current.rule) || text.trim(), rule: current.rule });
      onClose();
    }
  };

  return (
    <Sheet title="Repeat" visible={visible} onClose={done} {...(value && { onClear: () => (onChange(undefined), onClose()) })}>
      {PRESETS.map((p) => (
        <Option key={p.rule} label={p.label} on={text === p.rule} onPress={() => setText(p.rule)} />
      ))}
      <Text style={s.hint}>On these days</Text>
      <View style={s.row}>
        {DAYS.map((d) => {
          const on = days.includes(d);
          return (
            <Pressable key={d} style={[s.pill, on && s.pillOn]} onPress={() => toggleDay(d)} accessibilityState={{ selected: on }}>
              <Text style={[s.pillText, on && s.optionTextOn]}>{d[0]!.toUpperCase() + d.slice(1)}</Text>
            </Pressable>
          );
        })}
      </View>
      <Text style={s.hint}>Or after you complete it</Text>
      <View style={[s.row, { alignItems: "center" }]}>
        <Pressable style={s.pill} onPress={() => setAfter((a) => ({ ...a, n: Math.max(1, a.n - 1) }))} accessibilityLabel="Fewer">
          <Text style={s.pillText}>−</Text>
        </Pressable>
        <Text style={s.pillText}>{after.n}</Text>
        <Pressable style={s.pill} onPress={() => setAfter((a) => ({ ...a, n: Math.min(365, a.n + 1) }))} accessibilityLabel="More">
          <Text style={s.pillText}>+</Text>
        </Pressable>
        {(["d", "w"] as const).map((u) => (
          <Pressable key={u} style={[s.pill, after.unit === u && s.pillOn]} onPress={() => setAfter((a) => ({ ...a, unit: u }))}>
            <Text style={[s.pillText, after.unit === u && s.optionTextOn]}>{u === "d" ? "days" : "weeks"}</Text>
          </Pressable>
        ))}
        <Pressable style={[s.pill, text === `${after.n}${after.unit}` && s.pillOn]} onPress={() => setText(`${after.n}${after.unit}`)}>
          <Text style={[s.pillText, text === `${after.n}${after.unit}` && s.optionTextOn]}>Use</Text>
        </Pressable>
      </View>
      <TextInput
        style={s.input}
        value={text}
        onChangeText={setText}
        placeholder="Or type a rule: daily, mon,thu, 15th, 2w"
        placeholderTextColor={t.dim}
        autoCapitalize="none"
        autoCorrect={false}
        onSubmitEditing={done}
        submitBehavior="submit"
      />
      {text.trim() && current ? (
        current.rule ? (
          <Text style={s.hint}>→ {describeRule(current.rule)}</Text>
        ) : (
          <Text style={s.warn}>{current.warning || "Not a repeat rule"}</Text>
        )
      ) : null}
    </Sheet>
  );
}
