//! No wire type prints a credential under `Debug`.
//!
//! A `{:?}` reaches logs, panic messages and `assert_eq!` failures, so every
//! type that carries a daemon token, a device token, an invite secret or a
//! pairing URI renders `<redacted>` in its place. Each case formats the value
//! both ways (`{:?}` and `{:#?}`) and asserts the secret's bytes are absent
//! while a neighbouring field still prints, so the redaction is not a blank.

use ainb_hangar_proto::Redacted;
use ainb_hangar_proto::auth::{DeviceInfo, HelloParams};
use ainb_hangar_proto::devices::{
    DeviceInviteCreateResult, DeviceRedeemParams, DeviceRedeemResult, DeviceScope,
};
use ainb_hangar_proto::hosts::HostId;
use ainb_hangar_proto::protocol::ProtocolRange;

const DAEMON_TOKEN: &str = "mdt_s3cr3tDaemonTokenValue";
const DEVICE_TOKEN: &str = "mdd_s3cr3tDeviceTokenValue";
const INVITE_SECRET: &str = "c2VjcmV0SW52aXRlU2VjcmV0Qnl0ZXM";
const OFFER_URI: &str = "ainb://pair#eyJzZWNyZXRQYWlyaW5nUGF5bG9hZCI6dHJ1ZX0";

/// Assert neither rendering of `value` contains `secret`, and both contain
/// the redaction marker and `visible`.
fn assert_redacted(value: &impl std::fmt::Debug, secret: &str, visible: &str) {
    for rendered in [format!("{value:?}"), format!("{value:#?}")] {
        assert!(
            !rendered.contains(secret),
            "secret leaked in Debug: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(
            rendered.contains(visible),
            "redaction hid a non-secret field: {rendered}"
        );
    }
}

#[test]
fn the_marker_prints_only_redacted() {
    assert_eq!(format!("{Redacted:?}"), "<redacted>");
}

#[test]
fn hello_params_redacts_the_token() {
    let mut params: HelloParams = serde_json::from_value(serde_json::json!({
        "token": DAEMON_TOKEN,
    }))
    .unwrap();
    params.device = Some(DeviceInfo {
        device_id: "dev-visible".to_string(),
        display_name: None,
    });
    assert_redacted(&params, DAEMON_TOKEN, "dev-visible");
    let request = ainb_hangar_proto::auth::hello_request(1, DAEMON_TOKEN);
    let params: HelloParams = serde_json::from_value(request.params).unwrap();
    assert_redacted(&params, DAEMON_TOKEN, "capabilities");
}

#[test]
fn redeem_params_redact_the_invite_secret() {
    let params = DeviceRedeemParams {
        invite_id: "inv-visible".to_string(),
        invite_secret: INVITE_SECRET.to_string(),
        display_name: "phone".to_string(),
        protocol: ProtocolRange::supported(),
    };
    assert_redacted(&params, INVITE_SECRET, "inv-visible");
}

#[test]
fn redeem_result_redacts_the_device_token() {
    let result = DeviceRedeemResult {
        device_id: "dev-visible".to_string(),
        device_token: DEVICE_TOKEN.to_string(),
        scope: DeviceScope::MOBILE,
        expires_at_ms: 1,
        host_id: HostId::parse("01K5A0000000000000000ABCDE").unwrap(),
    };
    assert_redacted(&result, DEVICE_TOKEN, "dev-visible");
}

/// The offer URI carries the invite secret in its fragment, so the whole
/// URI is the secret here.
#[test]
fn invite_create_result_redacts_the_offer_uri() {
    let result = DeviceInviteCreateResult {
        offer: OFFER_URI.to_string(),
        invite_id: "inv-visible".to_string(),
        expires_at_ms: 1,
    };
    assert_redacted(&result, OFFER_URI, "inv-visible");
    assert_redacted(
        &result,
        "eyJzZWNyZXRQYWlyaW5nUGF5bG9hZCI6dHJ1ZX0",
        "inv-visible",
    );
}

/// Redaction is Debug-only: the wire still carries every secret verbatim.
#[test]
fn the_wire_is_unchanged() {
    let result = DeviceRedeemResult {
        device_id: "d".to_string(),
        device_token: DEVICE_TOKEN.to_string(),
        scope: DeviceScope::MOBILE,
        expires_at_ms: 1,
        host_id: HostId::parse("01K5A0000000000000000ABCDE").unwrap(),
    };
    assert_eq!(
        serde_json::to_value(&result).unwrap()["device_token"],
        DEVICE_TOKEN
    );
    let params: HelloParams =
        serde_json::from_value(serde_json::json!({"token": DAEMON_TOKEN})).unwrap();
    assert_eq!(
        serde_json::to_value(&params).unwrap()["token"],
        DAEMON_TOKEN
    );
}
