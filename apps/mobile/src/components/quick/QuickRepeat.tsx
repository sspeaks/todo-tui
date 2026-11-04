import { describeRule } from "@todo-tui/present";
import { useState } from "react";
import { type RepeatChoice, RepeatSheet } from "../pickers/RepeatSheet";
import { REPEAT_PRESETS, type RepeatPreset } from "./presets";
import { Pill } from "./Pill";

/**
 * A repeat in one tap: Daily, Weekdays, Weekly, Monthly (the chosen one
 * again stops it); "Custom…" opens the sheet for any other rule, which then
 * shows as its own chosen pill.
 */
export function QuickRepeat({
  preset,
  custom,
  anchor,
  current,
  onPreset,
  onChange,
}: {
  /** The preset the rule is, if it is one. */
  preset: RepeatPreset | undefined;
  /** How a rule that is no preset reads: "every Mon, Thu". */
  custom: string | undefined;
  anchor: number | undefined;
  /** The rule now, as text, for the sheet. */
  current: string | undefined;
  /** A preset tapped (undefined: the chosen one tapped again). */
  onPreset: (text: RepeatPreset | undefined) => void;
  onChange: (choice: RepeatChoice | undefined) => void;
}) {
  const [sheet, setSheet] = useState(false);
  return (
    <>
      {REPEAT_PRESETS.map((p) => (
        <Pill
          key={p.text}
          label={p.label}
          on={preset === p.text}
          onPress={() => onPreset(preset === p.text ? undefined : p.text)}
          accessibilityLabel={`Repeat ${p.label}`}
        />
      ))}
      <Pill icon="options-outline" label={custom ?? "Custom…"} on={!!custom} onPress={() => setSheet(true)} accessibilityLabel="Repeat: other" />
      {sheet ? <RepeatSheet visible value={current} anchor={anchor} onChange={onChange} onClose={() => setSheet(false)} /> : null}
    </>
  );
}

export const customLabel = (choice: RepeatChoice | undefined): string | undefined => (choice ? describeRule(choice.rule) : undefined);
