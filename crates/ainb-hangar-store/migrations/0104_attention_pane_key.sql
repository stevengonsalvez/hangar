-- Hangar v1 schema, migration 0104: the pane an attention row was raised in.
--
-- ainb hands every agent it launches AINB_PANE_KEY, the tmux pane id (%N) the
-- agent runs in, and the hook carries it on every line. Kept beside the
-- attention row, by the row's id, so an answer is typed into that pane
-- exactly, even for a row that names no provider session id: a pane id is
-- stable for the pane's life where an index (session:window.pane) is
-- renumbered when a lower pane closes. Absent for a row raised without the
-- key.

CREATE TABLE attention_pane (
    attention_id TEXT PRIMARY KEY REFERENCES attention(id) ON DELETE CASCADE,
    pane_key TEXT NOT NULL
);
