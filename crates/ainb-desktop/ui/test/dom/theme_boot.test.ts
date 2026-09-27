// `public/theme-boot.js`, the classic script in <head> that paints the stored
// theme before the app's own script runs (no wrong-theme flash on launch). It
// cannot import `resolveTheme`, so it is run here for every stored value, every
// system preference and a storage that throws, and must agree with it.

import "./window.ts";

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { resolveTheme, type ThemePreference } from "../../src/theme/theme.ts";

const BOOT = readFileSync(new URL("../../public/theme-boot.js", import.meta.url), "utf8");

function boot(stored: string | null, systemDark: boolean, storageThrows = false): string {
  document.documentElement.className = "";
  const storage = {
    getItem: (key: string) => {
      if (storageThrows) throw new Error("blocked");
      return key === "ainb.theme" ? stored : null;
    },
  };
  Object.defineProperty(window, "localStorage", { value: storage, configurable: true, writable: true });
  Object.defineProperty(window, "matchMedia", {
    value: () => ({ matches: systemDark }),
    configurable: true,
    writable: true,
  });
  new Function("window", "document", BOOT)(window, document);
  const classes = [...document.documentElement.classList].filter((name) => name === "dark" || name === "light");
  assert.equal(classes.length, 1, `exactly one theme class, got ${classes.join(",")}`);
  return classes[0];
}

test("the boot script paints what resolveTheme would, for every stored value and system", () => {
  for (const stored of ["system", "light", "dark"] as ThemePreference[]) {
    for (const systemDark of [true, false]) {
      assert.equal(boot(stored, systemDark), resolveTheme(stored, systemDark), `${stored} / system dark ${systemDark}`);
    }
  }
});

test("nothing stored, a value it does not know, or a blocked storage all follow the system", () => {
  for (const systemDark of [true, false]) {
    const expected = resolveTheme("system", systemDark);
    assert.equal(boot(null, systemDark), expected);
    assert.equal(boot("solarized", systemDark), expected);
    assert.equal(boot("light", systemDark, true), expected);
  }
});
