import assert from "node:assert/strict";
import { test } from "node:test";
import { accelerator } from "./tabs.ts";
import {
  MAX_FONT_SIZE,
  MIN_FONT_SIZE,
  TERMINAL_FONT_SIZE,
  nextFontSize,
  zoomChord,
  type ZoomDirection,
} from "./terminal_zoom.ts";

type Chord = { code: string; metaKey?: boolean; ctrlKey?: boolean; shiftKey?: boolean; altKey?: boolean };

const key = (chord: Chord) => ({ metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, ...chord });
const cmd = (code: string, shiftKey = false) => key({ code, metaKey: true, shiftKey });
const ctrl = (code: string, shiftKey = false) => key({ code, ctrlKey: true, shiftKey });
const ctrlShift = (code: string) => ctrl(code, true);

test("Orca's bounds and step: 8 to 32 points, one point a press", () => {
  assert.equal(MIN_FONT_SIZE, 8);
  assert.equal(MAX_FONT_SIZE, 32);
  assert.equal(nextFontSize(13, "in"), 14);
  assert.equal(nextFontSize(13, "out"), 12);
});

test("zooming stops at the bounds instead of passing them", () => {
  assert.equal(nextFontSize(MAX_FONT_SIZE, "in"), MAX_FONT_SIZE);
  assert.equal(nextFontSize(MIN_FONT_SIZE, "out"), MIN_FONT_SIZE);
  let size = TERMINAL_FONT_SIZE;
  for (let press = 0; press < 100; press += 1) size = nextFontSize(size, "in");
  assert.equal(size, MAX_FONT_SIZE);
  for (let press = 0; press < 100; press += 1) size = nextFontSize(size, "out");
  assert.equal(size, MIN_FONT_SIZE);
});

test("reset goes back to the pane's base size from anywhere", () => {
  for (const size of [MIN_FONT_SIZE, 12, TERMINAL_FONT_SIZE, 20, MAX_FONT_SIZE]) {
    assert.equal(nextFontSize(size, "reset"), TERMINAL_FONT_SIZE);
  }
});

test("macOS: Cmd+= and Cmd+Shift+= (Cmd++) zoom in, Cmd+- out, Cmd+0 resets, numpad too", () => {
  const cases: [Chord, ZoomDirection][] = [
    [cmd("Equal"), "in"],
    [cmd("Equal", true), "in"],
    [cmd("NumpadAdd"), "in"],
    [cmd("Minus"), "out"],
    [cmd("NumpadSubtract"), "out"],
    [cmd("Digit0"), "reset"],
  ];
  for (const [chord, direction] of cases) assert.equal(zoomChord(key(chord), true), direction, chord.code);
});

test("off macOS Mod is Ctrl, as in Orca: Ctrl+=, Ctrl++, Ctrl+-, Ctrl+0, numpad too", () => {
  const cases: [Chord, ZoomDirection][] = [
    [ctrl("Equal"), "in"],
    [ctrl("Equal", true), "in"],
    [ctrl("NumpadAdd"), "in"],
    [ctrl("Minus"), "out"],
    [ctrl("NumpadSubtract"), "out"],
    [ctrl("Digit0"), "reset"],
  ];
  for (const [chord, direction] of cases) assert.equal(zoomChord(key(chord), false), direction, chord.code);
});

test("Ctrl+Shift+- is Ctrl+_, the shell's undo, so it is never zoom out", () => {
  assert.equal(zoomChord(ctrl("Minus", true), false), null);
  assert.equal(zoomChord(ctrl("Digit0", true), false), null);
});

test("the other platform's form, Alt, and a stray modifier are not zoom", () => {
  assert.equal(zoomChord(ctrl("Equal"), true), null, "Ctrl on macOS");
  assert.equal(zoomChord(cmd("Equal"), false), null, "Meta off macOS");
  assert.equal(zoomChord(key({ code: "Equal", metaKey: true, altKey: true }), true), null);
  assert.equal(zoomChord(key({ code: "Equal", ctrlKey: true, altKey: true }), false), null);
  assert.equal(zoomChord(key({ code: "Minus", metaKey: true, ctrlKey: true }), true), null);
  assert.equal(zoomChord(key({ code: "Minus", metaKey: true, ctrlKey: true }), false), null);
  assert.equal(zoomChord(cmd("Minus", true), true), null, "Orca binds no Cmd+Shift+-");
  assert.equal(zoomChord(cmd("Digit0", true), true), null);
  assert.equal(zoomChord(key({ code: "Equal" }), true), null, "a bare = is typing");
});

test("zoom takes no shell chord (Cmd+U, Cmd+K, the tab chords), and the shell takes none of its", () => {
  for (const mac of [true, false]) {
    const chord = (code: string) => (mac ? cmd(code) : ctrlShift(code));
    for (const code of ["KeyU", "KeyF", "KeyK", "KeyW", "KeyN", "Digit1", "BracketLeft", "KeyC", "KeyV"]) {
      assert.equal(zoomChord(chord(code), mac), null, `${code} on mac=${mac}`);
    }
    const zoomForm = (code: string) => (mac ? cmd(code) : ctrl(code));
    for (const code of ["Equal", "Minus", "Digit0", "NumpadAdd", "NumpadSubtract"]) {
      assert.equal(accelerator(zoomForm(code), mac), null, `the shell leaves ${code} alone on mac=${mac}`);
      assert.equal(accelerator(chord(code), mac), null, `and its Shift form on mac=${mac}`);
    }
  }
});
