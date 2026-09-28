// Settings > Appearance > Theme, mounted: the switch shows the preference in
// use, a pick changes what the window paints at once and is kept for the next
// launch, and while the pick is System the window follows the OS live.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import { startTheme, THEME_KEY } from "../../src/theme/theme.ts";
import { ThemeSwitch } from "../../src/theme/theme_switch.tsx";

// `startTheme` holds transitions off for two frames around a switch.
const dom = window as unknown as { requestAnimationFrame: typeof requestAnimationFrame };
Object.defineProperty(globalThis, "requestAnimationFrame", {
  value: dom.requestAnimationFrame.bind(window),
  configurable: true,
  writable: true,
});

/** A controllable `prefers-color-scheme: dark` query, as the OS would drive it. */
function fakeSystem(dark: boolean) {
  const listeners: (() => void)[] = [];
  const query = {
    matches: dark,
    addEventListener: (_: string, listener: () => void) => listeners.push(listener),
  };
  Object.defineProperty(window, "matchMedia", { value: () => query, configurable: true, writable: true });
  return {
    set(next: boolean) {
      query.matches = next;
      for (const listener of listeners) listener();
    },
  };
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
  window.localStorage.clear();
  document.documentElement.className = "";
});

const radio = (value: string) =>
  document.querySelector<HTMLInputElement>(`.theme-choice[data-theme-choice="${value}"] input[type="radio"]`)!;
const painted = () => (document.documentElement.classList.contains("light") ? "light" : "dark");

async function mount(tellHost?: (preference: string) => void) {
  const theme = startTheme(tellHost);
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(ThemeSwitch, {
        get value() {
          return theme.preference();
        },
        onChange: theme.set,
      }),
    container,
  );
  await settle();
  return theme;
}

test("the switch is three native radios, System, Light and Dark, with the preference in use checked", async () => {
  fakeSystem(true);
  await mount();
  const radios = [...document.querySelectorAll<HTMLInputElement>('.theme-switch input[type="radio"]')];
  assert.deepEqual(
    radios.map((input) => input.value),
    ["system", "light", "dark"],
  );
  assert.equal(new Set(radios.map((input) => input.name)).size, 1, "one group, so the arrow keys move the choice");
  assert.equal(radio("system").checked, true, "nothing stored: the system's");
});

test("picking Light paints light at once, the terminal follows, and the pick survives a relaunch", async () => {
  fakeSystem(true);
  const theme = await mount();
  radio("light").click();
  await settle();
  assert.equal(painted(), "light");
  assert.equal(theme.painted(), "light", "the terminals read this");
  assert.equal(radio("light").checked, true);
  assert.equal(window.localStorage.getItem(THEME_KEY), "light");

  cleanup?.();
  const again = await mount();
  assert.equal(again.preference(), "light");
  assert.equal(painted(), "light");
});

test("on System the window follows the OS live; on an explicit pick it does not", async () => {
  const os = fakeSystem(true);
  const theme = await mount();
  assert.equal(painted(), "dark");
  os.set(false);
  await settle();
  assert.equal(painted(), "light", "System follows the OS turning light");
  assert.equal(theme.painted(), "light");

  radio("dark").click();
  await settle();
  os.set(false);
  await settle();
  assert.equal(painted(), "dark", "an explicit Dark ignores the OS");
});

test("the host hears the stored pick at start and every pick after, for the next launch's first frame", async () => {
  fakeSystem(true);
  window.localStorage.setItem(THEME_KEY, "light");
  const told: string[] = [];
  await mount((preference) => told.push(preference));
  assert.deepEqual(told, ["light"], "a pick made before the host kept one still reaches it");

  radio("dark").click();
  await settle();
  radio("system").click();
  await settle();
  assert.deepEqual(told, ["light", "dark", "system"]);
});
