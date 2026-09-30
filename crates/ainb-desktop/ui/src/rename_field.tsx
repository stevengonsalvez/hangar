import { createSignal, onMount, Show } from "solid-js";
import { label } from "./sessions.ts";

interface Props {
  /** The name the card shows now: the field's starting text. */
  current: string;
  /** Send `name` to the host as typed. Resolves `null` when the host kept
   * it, else the host's reason for refusing it. */
  onSubmit(name: string): Promise<string | null>;
  /** The field is done: the name was kept, or the edit was cancelled.
   * `restore` is true when the keyboard ended it (Enter, Esc) and should go
   * back to the row; false when it had already gone elsewhere (a blur). */
  onDone(restore: boolean): void;
}

/**
 * A card title turned into a field, the way Orca's inline rename behaves
 * (stablyai/orca@59b746f, `WorktreeTitleInlineRename.tsx`): it opens with the
 * whole name selected (`:160-168`), Enter commits and Esc cancels (`:254-260`),
 * and leaving it commits too (`:299`). A name equal to the one shown sends
 * nothing (`:18-21`). A refused name keeps the field open (`:224-234`), here
 * with the host's reason under it, since the host is what checks a name.
 *
 * Its keys stay in the field (`:245`): typing in it must not run a window
 * chord or open the row's menu.
 */
export function RenameField(props: Props) {
  let input!: HTMLInputElement;
  const [value, setValue] = createSignal(props.current);
  const [refusal, setRefusal] = createSignal<string | null>(null);
  // Read-only while a name is with the host, so nothing typed then is lost
  // when it answers; and a second Enter or a blur sends nothing more.
  const [sending, setSending] = createSignal(false);
  // A plain flag: nothing draws it. It stops a blur that follows Enter or Esc
  // (the field unmounting) from committing a second time.
  let settled = false;

  onMount(() => {
    input.focus();
    input.select();
  });

  const finish = (restore: boolean) => {
    settled = true;
    props.onDone(restore);
  };

  const commit = async (restore: boolean) => {
    if (sending() || settled) return;
    if (value() === props.current) return finish(restore);
    setSending(true);
    let why: string | null;
    try {
      why = await props.onSubmit(value());
    } catch (error) {
      why = String(error);
    } finally {
      setSending(false);
    }
    if (why === null) finish(restore);
    else setRefusal(why);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    event.stopPropagation();
    // An Enter that only confirms an input method's candidate is not a commit.
    if (event.isComposing) return;
    if (event.key === "Enter") {
      event.preventDefault();
      void commit(true);
    } else if (event.key === "Escape") {
      event.preventDefault();
      if (!sending()) finish(true);
    }
  };

  return (
    <div class="worktree-card-title renaming">
      <input
        ref={input}
        class="rename-input"
        type="text"
        value={value()}
        readOnly={sending()}
        spellcheck={false}
        aria-label="Rename worktree"
        aria-invalid={refusal() !== null ? "true" : undefined}
        onInput={(event) => {
          setValue(event.currentTarget.value);
          setRefusal(null);
        }}
        onKeyDown={onKeyDown}
        onBlur={() => void commit(false)}
        // The card's own right-click menu is not for the field.
        onContextMenu={(event) => event.stopPropagation()}
      />
      <Show when={refusal()}>{(why) => <p class="rename-refusal" role="alert">{label(why())}</p>}</Show>
    </div>
  );
}
