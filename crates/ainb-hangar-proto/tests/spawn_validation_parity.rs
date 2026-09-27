//! Every case in `fixtures/spawn_validation.json` through the daemon's own
//! `WorktreeCreateParams::validate`, and its `model` and `prompt` cases
//! through `WorktreeAgentAddParams::validate` too.
//!
//! The desktop composer validates the same fields before it sends anything,
//! and its test reads this same file. A rule changed on one side and not the
//! other fails one of the two tests instead of reaching a person as a form
//! that accepts what the daemon refuses.

use ainb_hangar_proto::mutation::MutationEnvelope;
use ainb_hangar_proto::spawn::{SpawnAgent, WorktreeAgentAddParams, WorktreeCreateParams};

fn base() -> WorktreeCreateParams {
    WorktreeCreateParams {
        repo_path: "/repos/app".into(),
        branch: None,
        base: None,
        agent: SpawnAgent::Claude,
        model: None,
        prompt: None,
        skip_permissions: false,
        mutation: MutationEnvelope::default(),
    }
}

#[test]
fn every_shared_case_matches_the_daemon_validator() {
    let raw = include_str!("fixtures/spawn_validation.json");
    let doc: serde_json::Value = serde_json::from_str(raw).expect("fixture parses");
    let cases = doc["cases"].as_array().expect("cases array");
    assert!(cases.len() > 30, "the fixture carries the full table");
    let mut wrong = Vec::new();
    for case in cases {
        let field = case["field"].as_str().expect("field");
        let value = case["value"].as_str().expect("value").to_string();
        let ok = case["ok"].as_bool().expect("ok");
        let mut params = base();
        match field {
            "branch" => params.branch = Some(value.clone()),
            "base" => params.base = Some(value.clone()),
            "model" => params.model = Some(value.clone()),
            "prompt" => params.prompt = Some(value.clone()),
            "repo_path" => params.repo_path = value.clone(),
            other => panic!("unknown field in fixture: {other}"),
        }
        if params.validate().is_ok() != ok {
            wrong.push(format!(
                "{field}={:?} expected ok={ok} ({})",
                value.chars().take(40).collect::<String>(),
                case["why"]
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "daemon validator disagrees with the shared cases:\n{}",
        wrong.join("\n")
    );
}

/// `worktree/agent_add` shares the `model` and `prompt` fields, so it answers
/// to the same cases for them. Its `worktree_path` has no cases here: the
/// desktop's runner reads this file too and refuses a field it does not know.
#[test]
fn agent_add_answers_the_shared_model_and_prompt_cases_the_same_way() {
    let raw = include_str!("fixtures/spawn_validation.json");
    let doc: serde_json::Value = serde_json::from_str(raw).expect("fixture parses");
    let cases = doc["cases"].as_array().expect("cases array");
    let mut checked = 0;
    let mut wrong = Vec::new();
    for case in cases {
        let field = case["field"].as_str().expect("field");
        let value = case["value"].as_str().expect("value").to_string();
        let ok = case["ok"].as_bool().expect("ok");
        let mut params = WorktreeAgentAddParams {
            worktree_path: "/w/app".into(),
            agent: SpawnAgent::Claude,
            model: None,
            prompt: None,
            skip_permissions: false,
            mutation: MutationEnvelope::default(),
        };
        match field {
            "model" => params.model = Some(value.clone()),
            "prompt" => params.prompt = Some(value.clone()),
            _ => continue,
        }
        checked += 1;
        if params.validate().is_ok() != ok {
            wrong.push(format!(
                "{field}={:?} expected ok={ok} ({})",
                value.chars().take(40).collect::<String>(),
                case["why"]
            ));
        }
    }
    assert!(checked >= 6, "the fixture carries model and prompt cases");
    assert!(
        wrong.is_empty(),
        "agent_add validator disagrees with the shared cases:\n{}",
        wrong.join("\n")
    );
}
