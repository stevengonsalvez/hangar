import { For } from "solid-js";
import type { ThemePreference } from "./theme.ts";

/** The three choices, in Orca's order. */
const CHOICES: readonly { value: ThemePreference; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

/**
 * Settings > Appearance > Theme: System, Light or Dark, as Orca offers it. A
 * radio group, so the arrow keys and a screen reader treat it as one choice.
 */
export function ThemeSwitch(props: { value: ThemePreference; onChange(next: ThemePreference): void }) {
  return (
    <div class="theme-switch" role="radiogroup" aria-label="Theme">
      <For each={CHOICES}>
        {(choice) => (
          <button
            type="button"
            role="radio"
            class="theme-choice"
            data-theme-choice={choice.value}
            aria-checked={props.value === choice.value}
            onClick={() => props.onChange(choice.value)}
          >
            {choice.label}
          </button>
        )}
      </For>
    </div>
  );
}
