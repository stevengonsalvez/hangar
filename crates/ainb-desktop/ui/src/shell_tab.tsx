import { NO_SESSION } from "./shell_tab.ts";

interface Props {
  /** Whether a session is selected, whose worktree the terminal opens in. */
  ready: boolean;
  mac: boolean;
  onOpen(): void;
}

/** The strip's "New terminal" +: a plain shell in the selected worktree. */
export function NewTerminalButton(props: Props) {
  const chord = () => (props.mac ? "⌘T" : "Ctrl+Shift+T");
  return (
    <button
      type="button"
      class="tab-new-terminal"
      aria-label="New terminal"
      title={props.ready ? `New terminal (${chord()})` : NO_SESSION}
      disabled={!props.ready}
      onClick={() => props.onOpen()}
    >
      +
    </button>
  );
}
