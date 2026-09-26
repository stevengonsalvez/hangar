//! Held hook requests: the one blocking, structured reply path.
//!
//! A Claude `PermissionRequest`, or `PreToolUse` on `AskUserQuestion`, POSTs to
//! `/hook/claude/hold` and the connection stays open here until a human
//! decides, the deadline passes, or the agent moves on. An answer from any
//! surface (`attention/answer`, `fleet/action`) resolves the hold with a
//! [`HoldDecision`], and the daemon, never the client, renders the JSON the
//! hook prints.
//!
//! Every way a hold ends without a decision returns `{}`: Claude then shows its
//! own prompt and the keyboard decides. Nothing here ever auto-approves or
//! silently denies.
//!
//! Process memory, like the notifyd broker it replaces: a hold cannot outlive
//! the daemon, because the hook's HTTP connection dies with it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use ainb_hangar_proto::fleet::FleetQuestionAnswer;
use serde_json::{Value, json};
use tokio::sync::watch;

/// How long a hold waits for a human before answering `{}`. Under the
/// listener's own 600s hold deadline, so the sink retires its row before the
/// listener cuts the call, and under the script's 610s curl budget and
/// Claude's 620s hook timeout.
pub const HOLD_DEADLINE: Duration = Duration::from_secs(590);
/// Most holds live at once. A hold past this answers `{}` at once.
pub const MAX_HOLDS: usize = 32;

/// What a human decided. Closed: the hook output is rendered from this and
/// nothing a client sends is copied into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldDecision {
    /// Allow the permission request.
    Allow {
        /// Optional note shown to the agent.
        message: Option<String>,
    },
    /// Deny the permission request.
    Deny {
        /// Optional reason shown to the agent.
        message: Option<String>,
    },
    /// Answers to the held `AskUserQuestion`, one per question, as labels.
    Answers(Vec<(String, String)>),
    /// Hand the request back to the agent's own prompt.
    Release,
}

/// What the held request asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeldRequest {
    /// A permission request for one tool call.
    Permission,
    /// An `AskUserQuestion` tool call; `tool_input` is kept verbatim so the
    /// daemon can copy `questions` back and add only `answers`.
    Ask {
        /// The held tool input.
        tool_input: Value,
    },
}

impl HeldRequest {
    /// Read the request from a hook payload, if it is one that holds.
    #[must_use]
    pub fn from_payload(payload: &Value) -> Option<Self> {
        match payload.get("hook_event_name").and_then(Value::as_str)? {
            "PermissionRequest" => Some(Self::Permission),
            "PreToolUse"
                if payload.get("tool_name").and_then(Value::as_str) == Some("AskUserQuestion") =>
            {
                let tool_input = payload.get("tool_input")?.clone();
                tool_input
                    .get("questions")
                    .and_then(Value::as_array)
                    .filter(|q| !q.is_empty())?;
                Some(Self::Ask { tool_input })
            }
            _ => None,
        }
    }

    /// Build the decision for a structured answer, checked against what was
    /// actually asked.
    ///
    /// # Errors
    /// A sentence naming the first answer that does not fit: a question not
    /// asked, a label not offered, a missing question, or free text where the
    /// question offers none.
    pub fn decide_answers(&self, answers: &[FleetQuestionAnswer]) -> Result<HoldDecision, String> {
        let Self::Ask { tool_input } = self else {
            return Err("this request is a permission, not a question".to_string());
        };
        let questions = tool_input["questions"].as_array().cloned().unwrap_or_default();
        let mut out = Vec::new();
        for q in &questions {
            let text = q.get("question").and_then(Value::as_str).unwrap_or_default();
            let Some(a) = answers.iter().find(|a| a.question_id == text) else {
                return Err(format!("no answer for the question {text:?}"));
            };
            let labels: Vec<&str> = q
                .get("options")
                .and_then(Value::as_array)
                .map(|o| o.iter().filter_map(|o| o.get("label").and_then(Value::as_str)).collect())
                .unwrap_or_default();
            let multi = q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false);
            if let Some(free) = &a.text {
                // AskUserQuestion always offers an "Other" free-text slot.
                if !a.selected_options.is_empty() {
                    return Err(format!("both options and free text for {text:?}"));
                }
                out.push((text.to_string(), free.clone()));
                continue;
            }
            if a.selected_options.is_empty() || (!multi && a.selected_options.len() > 1) {
                return Err(format!("wrong number of options for {text:?}"));
            }
            if let Some(bad) = a.selected_options.iter().find(|s| !labels.contains(&s.as_str())) {
                return Err(format!("{bad:?} is not an option of {text:?}"));
            }
            out.push((text.to_string(), a.selected_options.join(", ")));
        }
        if let Some(extra) = answers.iter().find(|a| {
            !questions
                .iter()
                .any(|q| q.get("question").and_then(Value::as_str) == Some(a.question_id.as_str()))
        }) {
            return Err(format!("{:?} was not asked", extra.question_id));
        }
        Ok(HoldDecision::Answers(out))
    }

    /// The single-label shortcut `attention/answer` has always taken: one
    /// question, one offered label.
    ///
    /// # Errors
    /// As [`Self::decide_answers`], and when the request asks more than one
    /// question.
    pub fn decide_label(&self, label: &str) -> Result<HoldDecision, String> {
        let Self::Ask { tool_input } = self else {
            return decide_permission(label);
        };
        let questions = tool_input["questions"].as_array().cloned().unwrap_or_default();
        let [q] = questions.as_slice() else {
            return Err("this request asks several questions; send structured answers".to_string());
        };
        let text = q.get("question").and_then(Value::as_str).unwrap_or_default();
        self.decide_answers(&[FleetQuestionAnswer {
            question_id: text.to_string(),
            selected_options: vec![label.to_string()],
            text: None,
        }])
    }

    /// Render the hook's stdout for `decision`. `None` means print `{}`.
    #[must_use]
    pub fn render(&self, decision: &HoldDecision) -> Option<Vec<u8>> {
        let out = match (self, decision) {
            (_, HoldDecision::Release) => return None,
            (Self::Permission, HoldDecision::Allow { message }) => json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": without_none(json!({"behavior": "allow", "message": message})),
                }
            }),
            (Self::Permission, HoldDecision::Deny { message }) => json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": without_none(json!({"behavior": "deny", "message": message})),
                }
            }),
            (Self::Ask { tool_input }, HoldDecision::Answers(answers)) => {
                let mut input = tool_input.clone();
                let map: serde_json::Map<String, Value> =
                    answers.iter().map(|(q, a)| (q.clone(), Value::String(a.clone()))).collect();
                input["answers"] = Value::Object(map);
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "allow",
                        "permissionDecisionReason": "answered through the hangar daemon",
                        "updatedInput": input,
                    }
                })
            }
            (Self::Ask { .. }, HoldDecision::Deny { message }) => json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": message.clone().unwrap_or_else(|| "declined".into()),
                }
            }),
            // A mismatched pair (an Allow for a question) has no safe rendering.
            _ => return None,
        };
        Some(out.to_string().into_bytes())
    }
}

fn without_none(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.retain(|_, v| !v.is_null());
    }
    v
}

/// `allow` / `deny` from the words surfaces send today.
///
/// # Errors
/// Anything that is not clearly one of the two.
pub fn decide_permission(label: &str) -> Result<HoldDecision, String> {
    match label.trim().to_ascii_lowercase().as_str() {
        "allow" | "approve" | "yes" | "y" => Ok(HoldDecision::Allow { message: None }),
        "deny" | "reject" | "no" | "n" => Ok(HoldDecision::Deny { message: None }),
        other => Err(format!("{other:?} is neither allow nor deny")),
    }
}

struct Slot {
    attention_id: String,
    request: HeldRequest,
    tx: watch::Sender<Option<HoldDecision>>,
}

/// The live holds, keyed by the request's stable key.
#[derive(Default)]
pub struct HoldRegistry {
    slots: Mutex<HashMap<String, Slot>>,
}

/// A registered hold. Waiting on it yields the decision, or `None`.
pub struct Waiter {
    registry: &'static HoldRegistry,
    key: String,
    rx: watch::Receiver<Option<HoldDecision>>,
    request: HeldRequest,
}

/// The process's registry.
pub fn registry() -> &'static HoldRegistry {
    static REGISTRY: OnceLock<HoldRegistry> = OnceLock::new();
    REGISTRY.get_or_init(HoldRegistry::default)
}

impl HoldRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Register a hold for `key`, or join the live one. `None` when the
    /// registry is full.
    pub fn register(
        &'static self,
        key: &str,
        attention_id: &str,
        request: HeldRequest,
    ) -> Option<Waiter> {
        let mut slots = self.lock();
        if let Some(slot) = slots.get(key) {
            return Some(Waiter {
                registry: self,
                key: key.to_string(),
                rx: slot.tx.subscribe(),
                request: slot.request.clone(),
            });
        }
        if slots.len() >= MAX_HOLDS {
            return None;
        }
        let (tx, rx) = watch::channel(None);
        slots.insert(
            key.to_string(),
            Slot {
                attention_id: attention_id.to_string(),
                request: request.clone(),
                tx,
            },
        );
        Some(Waiter {
            registry: self,
            key: key.to_string(),
            rx,
            request,
        })
    }

    /// The live request held for an attention row.
    #[must_use]
    pub fn request_for(&self, attention_id: &str) -> Option<HeldRequest> {
        self.lock()
            .values()
            .find(|s| s.attention_id == attention_id)
            .map(|s| s.request.clone())
    }

    /// Deliver `decision` to every waiter on the row's hold and drop the hold.
    /// `false` when no live hold (every waiter already gone) took it.
    pub fn resolve(&self, attention_id: &str, decision: HoldDecision) -> bool {
        let mut slots = self.lock();
        let Some(key) = slots
            .iter()
            .find(|(_, s)| s.attention_id == attention_id)
            .map(|(k, _)| k.clone())
        else {
            return false;
        };
        let slot = slots.remove(&key).expect("found above");
        slot.tx.send(Some(decision)).is_ok()
    }

    /// End the hold for `key` without a decision (the agent moved on). Every
    /// waiter answers `{}`.
    pub fn cancel(&self, key: &str) {
        self.lock().remove(key);
    }

    /// Keys of the live holds for one session.
    #[must_use]
    pub fn keys_for_session(&self, session: &str) -> Vec<String> {
        let prefix = format!("{session}:");
        self.lock().keys().filter(|k| k.starts_with(&prefix)).cloned().collect()
    }

    /// The attention row a live hold is for.
    #[must_use]
    pub fn attention_for(&self, key: &str) -> Option<String> {
        self.lock().get(key).map(|s| s.attention_id.clone())
    }

    /// Live holds, for tests and health.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether no hold is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for Waiter {
    /// A waiter dropped before a decision (its connection cut, its future
    /// cancelled) frees the slot once it was the last one on that hold.
    fn drop(&mut self) {
        let mut slots = self.registry.lock();
        if slots.get(&self.key).is_some_and(|slot| slot.tx.receiver_count() <= 1) {
            slots.remove(&self.key);
        }
    }
}

impl Waiter {
    /// Wait up to `deadline` for a decision. On timeout the hold is dropped
    /// so a late answer finds nothing to resolve.
    pub async fn wait(mut self, deadline: Duration) -> Option<(HeldRequest, HoldDecision)> {
        let rx = &mut self.rx;
        let got = tokio::time::timeout(deadline, async {
            loop {
                if let Some(d) = rx.borrow_and_update().clone() {
                    return Some(d);
                }
                if rx.changed().await.is_err() {
                    return None;
                }
            }
        })
        .await
        .ok()
        .flatten();
        // Drop frees the slot when this was the last waiter on it.
        got.map(|d| (self.request.clone(), d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaked() -> &'static HoldRegistry {
        Box::leak(Box::default())
    }

    fn ask() -> HeldRequest {
        HeldRequest::from_payload(&json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "AskUserQuestion",
            "tool_input": {"questions": [{
                "question": "Colour?",
                "header": "C",
                "multiSelect": false,
                "options": [{"label": "Red"}, {"label": "Blue"}]
            }]}
        }))
        .unwrap()
    }

    #[test]
    fn only_permission_and_ask_payloads_hold() {
        assert_eq!(
            HeldRequest::from_payload(&json!({"hook_event_name": "PermissionRequest"})),
            Some(HeldRequest::Permission)
        );
        assert!(ask() != HeldRequest::Permission);
        for other in [
            json!({"hook_event_name": "PreToolUse", "tool_name": "Bash"}),
            json!({"hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "tool_input": {}}),
            json!({"hook_event_name": "Stop"}),
        ] {
            assert_eq!(HeldRequest::from_payload(&other), None, "{other}");
        }
    }

    #[test]
    fn approvals_render_the_documented_shape_and_no_updated_input() {
        let allow = HeldRequest::Permission.render(&HoldDecision::Allow { message: None }).unwrap();
        let v: Value = serde_json::from_slice(&allow).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert!(!String::from_utf8(allow).unwrap().contains("updatedInput"));
        let deny = HeldRequest::Permission
            .render(&HoldDecision::Deny {
                message: Some("no".into()),
            })
            .unwrap();
        let v: Value = serde_json::from_slice(&deny).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(v["hookSpecificOutput"]["decision"]["message"], "no");
        assert_eq!(HeldRequest::Permission.render(&HoldDecision::Release), None);
    }

    #[test]
    fn an_answer_copies_the_held_questions_and_adds_only_answers() {
        let req = ask();
        let decision = req.decide_label("Blue").unwrap();
        let out: Value = serde_json::from_slice(&req.render(&decision).unwrap()).unwrap();
        let input = &out["hookSpecificOutput"]["updatedInput"];
        let HeldRequest::Ask { tool_input } = &req else {
            unreachable!()
        };
        assert_eq!(input["questions"], tool_input["questions"]);
        assert_eq!(input["answers"]["Colour?"], "Blue");
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    #[test]
    fn answers_that_do_not_fit_the_question_are_refused() {
        let req = ask();
        assert!(req.decide_label("Green").is_err(), "not offered");
        assert!(
            req.decide_answers(&[FleetQuestionAnswer {
                question_id: "Other?".into(),
                selected_options: vec!["Red".into()],
                text: None,
            }])
            .is_err()
        );
        assert!(
            req.decide_answers(&[FleetQuestionAnswer {
                question_id: "Colour?".into(),
                selected_options: vec!["Red".into(), "Blue".into()],
                text: None,
            }])
            .is_err(),
            "two picks on a single-select"
        );
        assert!(
            req.decide_answers(&[
                FleetQuestionAnswer {
                    question_id: "Colour?".into(),
                    selected_options: vec!["Red".into()],
                    text: None,
                },
                FleetQuestionAnswer {
                    question_id: "Extra?".into(),
                    selected_options: vec![],
                    text: Some("x".into()),
                },
            ])
            .is_err(),
            "an extra answer"
        );
        assert!(HeldRequest::Permission.decide_label("maybe").is_err());
        assert_eq!(
            HeldRequest::Permission.decide_label("Approve"),
            Ok(HoldDecision::Allow { message: None })
        );
    }

    #[tokio::test]
    async fn a_duplicate_request_joins_the_live_hold_and_both_get_the_decision() {
        let reg = leaked();
        let a = reg.register("s:t1", "att-1", HeldRequest::Permission).unwrap();
        let b = reg.register("s:t1", "att-1", HeldRequest::Permission).unwrap();
        assert_eq!(reg.len(), 1);
        let (ra, rb) = (
            tokio::spawn(a.wait(Duration::from_secs(5))),
            tokio::spawn(b.wait(Duration::from_secs(5))),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(reg.resolve("att-1", HoldDecision::Allow { message: None }));
        for r in [ra.await.unwrap(), rb.await.unwrap()] {
            assert_eq!(r.unwrap().1, HoldDecision::Allow { message: None });
        }
        assert!(reg.is_empty());
        assert!(
            !reg.resolve("att-1", HoldDecision::Allow { message: None }),
            "one decision only"
        );
    }

    #[tokio::test]
    async fn a_dropped_waiter_frees_its_slot_only_when_it_was_the_last() {
        let reg = leaked();
        let a = reg.register("s:d1", "att-d1", HeldRequest::Permission).unwrap();
        let b = reg.register("s:d1", "att-d1", HeldRequest::Permission).unwrap();
        drop(a);
        assert_eq!(reg.len(), 1, "b still waits");
        drop(b);
        assert!(reg.is_empty());
    }

    #[tokio::test]
    async fn a_deadline_or_a_cancel_answers_nothing() {
        let reg = leaked();
        let w = reg.register("s:t2", "att-2", HeldRequest::Permission).unwrap();
        assert!(w.wait(Duration::from_millis(30)).await.is_none());
        assert!(reg.is_empty(), "a timed-out hold is dropped");
        let w = reg.register("s:t3", "att-3", HeldRequest::Permission).unwrap();
        let waiting = tokio::spawn(w.wait(Duration::from_secs(5)));
        tokio::time::sleep(Duration::from_millis(20)).await;
        reg.cancel("s:t3");
        assert!(waiting.await.unwrap().is_none());
    }

    #[test]
    fn the_registry_is_bounded() {
        let reg = leaked();
        // Live waiters hold their slots; a dropped one frees its slot.
        let mut live = Vec::new();
        for i in 0..MAX_HOLDS {
            live.push(
                reg.register(&format!("k{i}"), &format!("a{i}"), HeldRequest::Permission)
                    .unwrap(),
            );
        }
        assert!(reg.register("one-more", "a", HeldRequest::Permission).is_none());
        assert!(
            reg.register("k0", "a0", HeldRequest::Permission).is_some(),
            "joining is not a new slot"
        );
        drop(live);
        assert!(reg.is_empty());
    }
}
