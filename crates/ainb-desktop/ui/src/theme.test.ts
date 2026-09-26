import { strict as assert } from "node:assert";
import { test } from "node:test";
import {
  applyTheme,
  readPreference,
  resolveTheme,
  THEME_KEY,
  writePreference,
  type ThemeStorage,
} from "./theme/theme.ts";

function memory(initial: Record<string, string> = {}): ThemeStorage & { data: Record<string, string> } {
  const data = { ...initial };
  return {
    data,
    getItem: (key) => (key in data ? data[key] : null),
    setItem: (key, value) => {
      data[key] = value;
    },
  };
}

const throwing: ThemeStorage = {
  getItem: () => {
    throw new Error("blocked");
  },
  setItem: () => {
    throw new Error("blocked");
  },
};

test("system follows the platform; an explicit choice wins over it", () => {
  assert.equal(resolveTheme("system", true), "dark");
  assert.equal(resolveTheme("system", false), "light");
  assert.equal(resolveTheme("dark", false), "dark");
  assert.equal(resolveTheme("light", true), "light");
});

test("a stored choice round-trips", () => {
  const storage = memory();
  writePreference(storage, "light");
  assert.equal(storage.data[THEME_KEY], "light");
  assert.equal(readPreference(storage), "light");
});

test("missing, unknown or unreadable storage reads as system", () => {
  assert.equal(readPreference(undefined), "system");
  assert.equal(readPreference(memory()), "system");
  assert.equal(readPreference(memory({ [THEME_KEY]: "neon" })), "system");
  assert.equal(readPreference(throwing), "system");
});

test("a storage that throws on write does not break the switch", () => {
  assert.doesNotThrow(() => writePreference(throwing, "dark"));
});

test("exactly one of dark and light is on the root", () => {
  const classes = new Set<string>();
  const root = {
    classList: {
      toggle: (name: string, on: boolean) => {
        if (on) classes.add(name);
        else classes.delete(name);
        return on;
      },
    } as unknown as DOMTokenList,
  };
  applyTheme(root, "light");
  assert.deepEqual([...classes], ["light"]);
  applyTheme(root, "dark");
  assert.deepEqual([...classes], ["dark"]);
});

test("terminal colours come from the tokens, with a dark fallback per token", async () => {
  const { terminalColors } = await import("./theme/theme.ts");
  const tokens: Record<string, string> = { "--background": " #ffffff", "--foreground": "#0f172a" };
  const colors = terminalColors((token) => tokens[token] ?? "");
  assert.equal(colors.background, "#ffffff");
  assert.equal(colors.foreground, "#0f172a");
  assert.equal(colors.cursor, "rgb(96, 165, 250)", "an unset token falls back, never empty");
  assert.equal(colors.selectionBackground, "#1e2636");
});
