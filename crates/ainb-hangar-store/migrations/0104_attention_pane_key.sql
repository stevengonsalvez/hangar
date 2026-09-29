-- Hangar v1 schema, migration 0104: the pane an attention row was raised in.
--
-- The hook reads its own pane off TMUX_PANE and puts it on every line as the
-- process-start fingerprint: the pane's id (%N), the pid of what ran in it,
-- and when its tmux session was created. Kept beside the attention row, by
-- the row's id, so an answer is typed into that pane exactly, even for a row
-- that names no provider session id, and only while the pane still runs
-- that pid in that session: a pane id is stable for the pane's life where an
-- index (session:window.pane) is renumbered when a lower pane closes, a pane
-- ainb respawns keeps its id and gets a new pid, and a tmux server started
-- again hands out %0 again. Absent for a row raised outside tmux.

CREATE TABLE attention_pane (
    attention_id TEXT PRIMARY KEY REFERENCES attention(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL
);
