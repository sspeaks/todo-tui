import { useState } from "react";
import { LabelsSheet } from "../pickers/LabelsSheet";
import { Pill } from "./Pill";

/** The labels in use, each one tap on and off; "+#" for a new one. */
export function QuickLabels({ value, known, onChange }: { value: readonly string[]; known: readonly string[]; onChange: (labels: string[]) => void }) {
  const [adding, setAdding] = useState(false);
  const all = [...new Set([...value, ...known])].sort((a, b) => a.localeCompare(b));
  const toggle = (l: string) => onChange(value.includes(l) ? value.filter((x) => x !== l) : [...value, l]);
  return (
    <>
      {all.map((l) => (
        <Pill key={l} label={`#${l}`} on={value.includes(l)} onPress={() => toggle(l)} />
      ))}
      <Pill label="+ #" onPress={() => setAdding(true)} accessibilityLabel="New label" />
      {adding ? <LabelsSheet visible value={value} known={known} onChange={onChange} onClose={() => setAdding(false)} /> : null}
    </>
  );
}
