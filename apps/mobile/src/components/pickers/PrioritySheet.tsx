import { Priority } from "@todo-tui/client/portable";
import { Option, Sheet } from "./Sheet";

export const PRIORITIES: { value: Priority; token: string; name: string }[] = [
  { value: Priority.HIGHEST, token: "+1", name: "Highest" },
  { value: Priority.HIGH, token: "+2", name: "High" },
  { value: Priority.MEDIUM, token: "+3", name: "Medium" },
  { value: Priority.LOW, token: "+4", name: "Low" },
  { value: Priority.LOWEST, token: "+5", name: "Lowest" },
];

/** "+2 High"; undefined for none. */
export const priorityText = (p: Priority | undefined): string | undefined => {
  const found = PRIORITIES.find((x) => x.value === p);
  return found && `${found.token} ${found.name}`;
};

export function PrioritySheet({
  visible,
  value,
  onChange,
  onClose,
}: {
  visible: boolean;
  value: Priority | undefined;
  onChange: (p: Priority | undefined) => void;
  onClose: () => void;
}) {
  const choose = (p: Priority | undefined) => {
    onChange(p);
    onClose();
  };
  const set = value !== undefined && value !== Priority.NONE;
  return (
    <Sheet title="Priority" visible={visible} onClose={onClose} {...(set && { onClear: () => choose(undefined) })}>
      {PRIORITIES.map((p) => (
        <Option key={p.token} label={p.name} detail={p.token} on={value === p.value} onPress={() => choose(p.value)} />
      ))}
    </Sheet>
  );
}
