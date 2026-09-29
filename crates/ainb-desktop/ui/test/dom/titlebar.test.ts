// The titlebar mounted: its three buttons send exactly the click `main.tsx`
// wires, and macOS gets the traffic-light padding class.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import { Titlebar } from "../../src/titlebar.tsx";

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

async function open(over: Partial<Parameters<typeof Titlebar>[0]> = {}) {
  const calls = { search: 0, inbox: 0, settings: 0 };
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(Titlebar, {
        mac: false,
        searchOpen: false,
        onSearch: () => calls.search++,
        inboxOpen: false,
        inboxUnread: 0,
        onInbox: () => calls.inbox++,
        settingsOpen: false,
        onSettings: () => calls.settings++,
        ...over,
      }),
    container,
  );
  await settle();
  return { calls };
}

test("the bar drags the window; it names the app only on macOS", async () => {
  await open({ mac: true });
  const bar = document.querySelector("header.titlebar");
  assert.ok(bar?.hasAttribute("data-tauri-drag-region"), "the bar itself is the drag region");
  assert.match(document.querySelector(".titlebar-app")?.textContent ?? "", /ainb/i);

  // Elsewhere the native title bar already names the window.
  cleanup?.();
  document.body.innerHTML = "";
  await open({ mac: false });
  assert.equal(document.querySelector(".titlebar-app"), null);
});

test("the Search button's title names the palette's chord for the platform", async () => {
  await open({ mac: true });
  assert.equal(document.querySelector(".titlebar-search")?.getAttribute("title"), "Search (Cmd+J)");
  cleanup?.();
  document.body.innerHTML = "";
  await open({ mac: false });
  assert.equal(document.querySelector(".titlebar-search")?.getAttribute("title"), "Search (Ctrl+Shift+J)");
});

test("macOS gets the traffic-light padding class; other platforms do not", async () => {
  await open({ mac: true });
  assert.equal(document.querySelector("header.titlebar")?.classList.contains("mac"), true);
  cleanup?.();
  document.body.innerHTML = "";
  await open({ mac: false });
  assert.equal(document.querySelector("header.titlebar")?.classList.contains("mac"), false);
});

test("search opens the same palette the accelerator does", async () => {
  const { calls } = await open();
  document.querySelector<HTMLButtonElement>("button.titlebar-search")?.click();
  assert.equal(calls.search, 1);
});

test("the inbox button shows the unread badge and toggles pressed with the page", async () => {
  await open({ inboxUnread: 3, inboxOpen: true });
  assert.equal(document.querySelector(".inbox-unread")?.textContent, "3");
  assert.equal(document.querySelector("button.inbox-button")?.getAttribute("aria-pressed"), "true");
});

test("the settings button sends onSettings and reflects whether the page is open", async () => {
  const { calls } = await open({ settingsOpen: true });
  assert.equal(document.querySelector("button.settings")?.getAttribute("aria-pressed"), "true");
  document.querySelector<HTMLButtonElement>("button.settings")?.click();
  assert.equal(calls.settings, 1);
});
