import { create, type Person, PersonSchema } from "@todo-tui/client/portable";
import { describe, expect, it } from "vitest";
import { personEmail, personName } from "../src/item.js";

describe("personName", () => {
  const dana = create(PersonSchema, { id: "01JB8X4Z5Q6R7S8T9V0W1X2Y3Z", displayName: "Dana\nExample", emails: ["dana@example.com"] });
  const people = { get: (id: string): Person | undefined => (id === dana.id ? dana : undefined) };
  const byId = (value: string) => ({ ref: { case: "personId" as const, value } }) as Parameters<typeof personName>[0];

  it("names a Person by their display name, on one line, and never by their id", () => {
    expect(personName(byId(dana.id), people)).toBe("Dana Example");
    expect(personEmail(byId(dana.id), people)).toBe("dana@example.com");
    // Not known yet (the People are loading, or an older daemon).
    expect(personName(byId("01JB8X4Z5Q6R7S8T9V0W1X2Y40"), people)).toBe("someone");
    expect(personName(byId(dana.id))).toBe("someone");
    expect(personEmail(byId(dana.id))).toBe("");
  });

  it("keeps free text as typed, and nothing as nothing", () => {
    const text = { ref: { case: "freeText" as const, value: "the vendor" } } as Parameters<typeof personName>[0];
    expect(personName(text, people)).toBe("the vendor");
    expect(personEmail(text, people)).toBe("");
    expect(personName(undefined, people)).toBe("");
  });
});
