-- Hangar v1 schema, migration 0103: the Claude session id a launch runs under.
--
-- ainb mints a session id for every Claude launch and hands it to
-- `claude --session-id`, so the id the session's hooks report is one the
-- session record already holds: the daemon files every request the session
-- raises under that id, and a surface places the request on the session by it
-- exactly, never by worktree. Carried here so the record survives the round
-- trip through the daemon. NULL for a session launched before this column, or
-- for any other agent.

ALTER TABLE sessions ADD COLUMN claude_session_id TEXT;
