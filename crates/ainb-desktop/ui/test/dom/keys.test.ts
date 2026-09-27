// The window's keydown handler (`shellKeydown`, the one `main.tsx` installs),
// driven with real key events on a focused text field.
//
// Off macOS, Ctrl+Shift+C and Ctrl+Shift+V are the shell's copy and paste.
// Under the new-worktree composer they are chords the modal refuses, and a
// refused chord must be left alone entirely: not run, and not taken, so the
// field the person is typing in still copies and pastes.

import "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { shellKeydown, type Accelerator } from "../../src/tabs.ts";

let detach: (() => void) | null = null;

afterEach(() => {
  detach?.();
  detach = null;
  document.body.replaceChildren();
});

/** A focused field under `shellKeydown`, and what the handler ran. */
function world(modalOpen: boolean) {
  const ran: Accelerator[] = [];
  const onKey = shellKeydown({ mac: false, modalOpen: () => modalOpen, run: (shell) => ran.push(shell) });
  window.addEventListener("keydown", onKey);
  detach = () => window.removeEventListener("keydown", onKey);
  const field = document.createElement("input");
  field.className = "composer-name";
  document.body.append(field);
  field.focus();
  return { field, ran };
}

function press(target: HTMLElement, code: string) {
  const event = new window.KeyboardEvent("keydown", {
    code,
    key: code.replace("Key", "").toLowerCase(),
    ctrlKey: true,
    shiftKey: true,
    bubbles: true,
    cancelable: true,
  });
  target.dispatchEvent(event);
  return event;
}

test("under the composer, Ctrl+Shift+C and Ctrl+Shift+V reach the field", () => {
  const { field, ran } = world(true);
  for (const code of ["KeyC", "KeyV"]) {
    const event = press(field, code);
    assert.equal(event.defaultPrevented, false, `${code} was taken from the field`);
  }
  assert.deepEqual(ran, [], "and the shell ran neither");
});

test("with no modal open, the same chords are the shell's copy and paste", () => {
  const { field, ran } = world(false);
  assert.equal(press(field, "KeyC").defaultPrevented, true);
  assert.equal(press(field, "KeyV").defaultPrevented, true);
  assert.deepEqual(ran, [{ kind: "copy" }, { kind: "paste" }]);
});

test("the one chord the composer allows still runs under it", () => {
  const { field, ran } = world(true);
  assert.equal(press(field, "KeyN").defaultPrevented, true);
  assert.deepEqual(ran, [{ kind: "new" }]);
});
