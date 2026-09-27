// Settings > Appearance > Theme, mounted: the switch shows the preference in
// use, a pick changes what the window paints at once, and the pick is kept
// for the next launch. Before this, `startTheme`'s setter was dropped, so the
// window had no way to change theme at all.

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

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
  window.localStorage.clear();
  document.documentElement.className = "";
});

const choice = (value: string) =>
  document.querySelector<HTMLButtonElement>(`.theme-choice[data-theme-choice="${value}"]`)!;

async function mount() {
  const theme = startTheme();
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

test("the switch offers System, Light and Dark, with the preference in use checked", async () => {
  await mount();
  const group = document.querySelector('[role="radiogroup"][aria-label="Theme"]');
  assert.ok(group, "one radio group named Theme");
  assert.deepEqual(
    [...document.querySelectorAll(".theme-choice")].map((button) => button.textContent),
    ["System", "Light", "Dark"],
  );
  assert.equal(choice("system").getAttribute("aria-checked"), "true", "nothing stored: the system's");
});

test("picking Light paints light at once, checks it, and keeps it for the next launch", async () => {
  await mount();
  choice("light").click();
  await settle();
  assert.equal(document.documentElement.classList.contains("light"), true);
  assert.equal(document.documentElement.classList.contains("dark"), false);
  assert.equal(choice("light").getAttribute("aria-checked"), "true");
  assert.equal(choice("system").getAttribute("aria-checked"), "false");
  assert.equal(window.localStorage.getItem(THEME_KEY), "light");

  // The next launch starts on the stored pick.
  cleanup?.();
  const again = await mount();
  assert.equal(again.preference(), "light");
  assert.equal(document.documentElement.classList.contains("light"), true);
});

test("picking Dark then System follows the pick each time", async () => {
  const theme = await mount();
  choice("dark").click();
  await settle();
  assert.equal(document.documentElement.classList.contains("dark"), true);
  choice("system").click();
  await settle();
  assert.equal(theme.preference(), "system");
  assert.equal(window.localStorage.getItem(THEME_KEY), "system");
});
