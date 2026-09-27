import { For } from "solid-js";
import type { ThemePreference } from "./theme.ts";

/** The three choices, in Orca's order. */
const CHOICES: readonly { value: ThemePreference; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

/**
 * Settings > Appearance > Theme: System, Light or Dark, as Orca offers it.
 * Native radio inputs under one name, drawn as a segmented control: the arrow
 * keys move the choice and a screen reader reads one group, with no keyboard
 * handling of our own to get wrong.
 */
export function ThemeSwitch(props: { value: ThemePreference; onChange(next: ThemePreference): void }) {
  return (
    <fieldset class="theme-switch">
      <legend class="visually-hidden">Theme</legend>
      <For each={CHOICES}>
        {(choice) => (
          <label class="theme-choice" data-theme-choice={choice.value}>
            <input
              type="radio"
              name="ainb-theme"
              value={choice.value}
              checked={props.value === choice.value}
              onChange={() => props.onChange(choice.value)}
            />
            <span>{choice.label}</span>
          </label>
        )}
      </For>
    </fieldset>
  );
}
