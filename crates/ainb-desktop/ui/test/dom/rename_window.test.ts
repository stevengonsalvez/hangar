// The whole window, mounted over the fake host (`window_host.ts`): the row
// menu's Rename turns the card's title into a field with the name selected,
// Enter sends the host exactly the session and the name, Esc sends nothing
// and restores the title, and a name the host refuses keeps the field open
// with the host's reason. The host checks names (`src/rename.rs`); these pin
// what the window does with its answers, and that it sends nothing else.

import "./window.ts";
import { drain, host, mountWindow, settingsShown, show, until } from "./window_host.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

const row = (id: string) => document.querySelector<HTMLElement>(`.session-row[data-session="${id}"]`);
const card = (id: string) => row(id)?.closest<HTMLElement>(".worktree-card") ?? null;
const title = (id: string) => card(id)?.querySelector<HTMLElement>(".worktree-card-title")?.textContent ?? "";
const field = () => document.querySelector<HTMLInputElement>(".rename-input");
const refusal = () => document.querySelector<HTMLElement>(".rename-refusal")?.textContent ?? null;
const inboxShown = () => document.querySelector(".inbox") !== null;
const toasts = () => [...document.querySelectorAll(".toast")].map((toast) => toast.textContent ?? "");

function focusIs(expected: Element | null, message: string) {
  const active = document.activeElement;
  assert.ok(active === expected, `${message}: focus is on ${active?.outerHTML.slice(0, 80) ?? "nothing"}`);
}

/** Right-click `id`'s row and choose Rename, as a person does. */
async function openRename(id: string): Promise<HTMLInputElement> {
  await until(() => row(id) !== null, "the sidebar row");
  row(id)!.focus();
  row(id)!.dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true }) as unknown as Event);
  await until(() => document.querySelector('[role="menu"]') !== null, "the row menu");
  const item = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find((el) => el.textContent === "Rename");
  assert.ok(item, "the menu offers Rename");
  item.click();
  await until(() => field() !== null, "the rename field");
  return field()!;
}

/** Replace the field's text, as typing does. */
function type(input: HTMLInputElement, text: string): void {
  input.value = text;
  input.dispatchEvent(new window.Event("input", { bubbles: true }) as unknown as Event);
}

function press(input: HTMLInputElement, key: string): KeyboardEvent {
  const event = new window.KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
  input.dispatchEvent(event as unknown as Event);
  return event as unknown as KeyboardEvent;
}

/** Start each test with no renames sent, every name kept, nothing reducer-bound recorded. */
function reset(): void {
  host.renames = [];
  host.sent = [];
  host.renameReply = () => null;
  host.renameGate = Promise.resolve();
}

test("Rename opens the card's title as a field, focused, with the whole name selected", async () => {
  await mountWindow();
  reset();
  const input = await openRename("u-1");
  focusIs(input, "the field takes the keyboard");
  assert.equal(input.value, "u-1", "it starts from the name the card shows");
  assert.equal(input.selectionStart, 0);
  assert.equal(input.selectionEnd, "u-1".length, "the whole name is selected, one keystroke replaces it");
  assert.deepEqual(host.renames, [], "opening sends nothing");
  press(input, "Escape");
  await until(() => field() === null, "Esc to close it");
});

test("Enter sends exactly the session and the name, and the card shows the host's label", async () => {
  reset();
  const input = await openRename("u-1");
  type(input, "Fix login");
  const enter = press(input, "Enter");
  assert.equal(enter.defaultPrevented, true);
  await until(() => field() === null, "the field to close once the host kept the name");
  assert.deepEqual(host.renames, [{ id: "u-1", name: "Fix login" }], "the id and the name, nothing else");
  assert.deepEqual(host.sent, [], "no reducer row and no walk home: the host keeps names itself");
  await until(() => title("u-1") === "Fix login", "the label's frame to retitle the card");
  focusIs(row("u-1"), "the keyboard goes back to the row");
});

test("Esc sends nothing and puts the title back as it was", async () => {
  reset();
  const before = title("u-2");
  const input = await openRename("u-2");
  type(input, "Something else");
  const escape = press(input, "Escape");
  assert.equal(escape.defaultPrevented, true);
  await until(() => field() === null, "Esc to close the field");
  await drain();
  assert.deepEqual(host.renames, [], "a cancel never reaches the host");
  assert.equal(title("u-2"), before);
  focusIs(row("u-2"), "the keyboard goes back to the row");
});

test("an empty name the host refuses keeps the field open and says why", async () => {
  reset();
  host.renameReply = (name) => (name.trim() === "" ? "A name cannot be empty." : null);
  const input = await openRename("u-2");
  type(input, "   ");
  press(input, "Enter");
  await until(() => refusal() !== null, "the host's reason");
  assert.equal(refusal(), "A name cannot be empty.");
  assert.ok(field() === input, "the same field stays open");
  assert.equal(input.value, "   ", "with what was typed, to fix");
  assert.equal(input.getAttribute("aria-invalid"), "true");
  assert.deepEqual(host.renames, [{ id: "u-2", name: "   " }], "sent as typed: the host trims and checks");
  type(input, "u-2 better");
  assert.equal(refusal(), null, "typing again clears the reason");
  press(input, "Escape");
  await until(() => field() === null, "Esc to close it");
});

test("a duplicate name the host refuses keeps the field open and says why", async () => {
  reset();
  const taken = 'Another worktree in this project is already named "u-1".';
  host.renameReply = (name) => (name.trim() === "u-1" ? taken : null);
  const input = await openRename("u-2");
  type(input, "u-1");
  press(input, "Enter");
  await until(() => refusal() !== null, "the host's reason");
  assert.equal(refusal(), taken);
  assert.ok(field() === input, "the field stays open");
  focusIs(input, "and keeps the keyboard");
  assert.notEqual(title("u-1"), "", "the other card is untouched");
  press(input, "Escape");
  await until(() => field() === null, "Esc to close it");
  assert.equal(host.renames.length, 1);
});

test("leaving the field commits it, as Orca's blur does; an unchanged name sends nothing", async () => {
  reset();
  let input = await openRename("u-3");
  input.blur();
  await until(() => field() === null, "the field to close on an unchanged blur");
  assert.deepEqual(host.renames, [], "the name it already has is not sent");

  input = await openRename("u-3");
  type(input, "Blurred");
  input.blur();
  await until(() => field() === null, "the field to close once the host kept it");
  assert.deepEqual(host.renames, [{ id: "u-3", name: "Blurred" }]);
});

test("while the host answers, the field is read-only and a second Enter or a blur sends nothing more", async () => {
  reset();
  const input = await openRename("u-3");
  type(input, "Once");
  press(input, "Enter");
  assert.equal(input.readOnly, true, "nothing typed now is lost when the host answers");
  press(input, "Enter");
  input.dispatchEvent(new window.FocusEvent("blur") as unknown as Event);
  await until(() => field() === null, "the field to close");
  await drain();
  assert.deepEqual(host.renames, [{ id: "u-3", name: "Once" }], "one rename, however it was left");
});

test("a rename the host answers late closes its own field, not the one opened since", async () => {
  reset();
  let answer!: () => void;
  host.renameGate = new Promise((resolve) => (answer = resolve));
  const first = await openRename("u-1");
  type(first, "Slow answer");
  first.blur();
  await until(() => host.renames.length === 1, "the first rename to reach the host");
  // The person moves on to another card while the host is still answering.
  card("u-2")!.querySelector<HTMLElement>(".worktree-card-title")!.dispatchEvent(
    new window.MouseEvent("dblclick", { bubbles: true }) as unknown as Event,
  );
  await until(() => card("u-2")?.querySelector(".rename-input") != null, "the second card's field");
  answer();
  await until(() => title("u-1") === "Slow answer", "the first card retitled");
  await drain();
  const second = card("u-2")?.querySelector<HTMLInputElement>(".rename-input");
  assert.ok(second, "the second card's field is still open");
  press(second, "Escape");
  await until(() => field() === null, "Esc to close it");
});

test("the blur that follows Esc sends nothing", async () => {
  reset();
  const input = await openRename("u-3");
  type(input, "Not this");
  press(input, "Escape");
  // The field unmounting takes the keyboard with it: a blur, after the cancel.
  input.dispatchEvent(new window.FocusEvent("blur") as unknown as Event);
  await until(() => field() === null, "the field to close");
  await drain();
  assert.deepEqual(host.renames, []);
});

test("a double-click on the title opens the field too, as Orca's does", async () => {
  reset();
  const heading = card("u-2")!.querySelector<HTMLElement>(".worktree-card-title")!;
  heading.dispatchEvent(new window.MouseEvent("dblclick", { bubbles: true }) as unknown as Event);
  await until(() => field() !== null, "the field");
  focusIs(field(), "focused");
  press(field()!, "Escape");
  await until(() => field() === null, "Esc to close it");
});

test("a key typed in the field never reaches the window's chords", async () => {
  reset();
  const input = await openRename("u-2");
  // The window's chords (Mod+N, the tab keys) listen on `window`; a key the
  // field keeps never gets there, as Orca's field stops it (`:245`).
  const heard: string[] = [];
  const listen = (event: Event) => heard.push((event as KeyboardEvent).key);
  window.addEventListener("keydown", listen);
  try {
    type(input, "u-2 kept");
    // Ctrl+N, then the Enter that commits: neither may reach the window.
    for (const key of ["n", "Enter"]) {
      input.dispatchEvent(
        new window.KeyboardEvent("keydown", { key, ctrlKey: key === "n", bubbles: true, cancelable: true }) as unknown as Event,
      );
      await drain();
    }
  } finally {
    window.removeEventListener("keydown", listen);
  }
  assert.deepEqual(heard, [], "no key typed in the field reached the window");
  await until(() => field() === null, "the field to close");
});

for (const [page, screen, shown] of [
  ["Settings", "config", settingsShown],
  ["the Inbox", "inbox", inboxShown],
] as const) {
  test(`Rename from the row menu over ${page} sends the name and leaves the page open`, async () => {
    show(screen);
    await until(shown, `${page} to show`);
    reset();
    const input = await openRename("u-2");
    type(input, `Over ${screen}`);
    press(input, "Enter");
    await until(() => field() === null, "the field to close");
    await drain();
    assert.deepEqual(host.renames, [{ id: "u-2", name: `Over ${screen}` }]);
    // Rename runs no session-list row, so there is nothing for the reducer's
    // screen gate to refuse and no reason to leave the page: unlike Open,
    // which goes through `answer` (home first), it goes straight to the host.
    assert.deepEqual(host.sent, [], "no answer_home, no dispatch");
    assert.ok(shown(), `${page} stays open`);
    assert.deepEqual(
      toasts().filter((text) => text.includes("is not run from the window")),
      [],
      "no refusal",
    );
    await until(() => title("u-2") === `Over ${screen}`, "the card retitled");
    show("session_list");
  });
}
