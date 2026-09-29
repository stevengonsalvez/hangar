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

test("the terminal palette follows the window theme, light being Orca's Tango Light", async () => {
  const { TERMINAL_THEMES } = await import("./theme/theme.ts");
  assert.equal(TERMINAL_THEMES.dark.background, "#0b0e14");
  assert.equal(TERMINAL_THEMES.dark.foreground, "rgb(226, 232, 240)");
  assert.equal(TERMINAL_THEMES.light.background, "#ffffff");
  assert.equal(TERMINAL_THEMES.light.foreground, "#2e3434");
  // Orca darkened the accents that sit on white, so agent CLI text stays legible.
  assert.equal(TERMINAL_THEMES.light.yellow, "#8e7700");
  assert.equal(TERMINAL_THEMES.light.white, "#6a6a6a");
  assert.equal(TERMINAL_THEMES.light.brightWhite, "#3d3d3d");
});

test("the terminal holds text to Orca's contrast floor: 4.5 on light, 3 on dark", async () => {
  const { TERMINAL_MIN_CONTRAST } = await import("./theme/theme.ts");
  assert.equal(TERMINAL_MIN_CONTRAST.light, 4.5);
  assert.equal(TERMINAL_MIN_CONTRAST.dark, 3);
});
