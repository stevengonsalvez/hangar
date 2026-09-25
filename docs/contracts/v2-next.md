# v2-next contract: federation (R1), terminal (R2), mobile (M1)

This is the human index of the frozen contract. The code is the contract; this
page says where each item lives and which test keeps it honest. After the
freeze, any change follows the D17 bump rule: a new optional member or method is
a capability string, a changed meaning or layout is a `PROTOCOL_VERSION` bump.

```
                ┌──────────────────────────┐
                │ ainb-hangar-proto        │  types, methods, scope table
                └────────────┬─────────────┘
                             │
          ┌──────────────────┼───────────────────┐
          ▼                  ▼                   ▼
 ┌─────────────────┐ ┌───────────────┐ ┌──────────────────┐
 │ ainb-hangar-    │ │ daemon        │ │ client, app,     │
 │ noise (peer v1) │ │ (dark stubs)  │ │ desktop, phone   │
 └─────────────────┘ └───────────────┘ └──────────────────┘
```

## Nothing ships behind the freeze

| Guard | Where | Test |
|---|---|---|
| Eight capabilities defined, none advertised | `crates/ainb-hangar-proto/src/protocol.rs` (`DARK_CAPABILITIES`) | `protocol::tests::the_v2_next_capabilities_are_dark`, `v2_contract_dark` |
| No new method in `MUTATING_METHODS` | `crates/ainb-hangar-proto/src/mutation.rs` | `mutation::tests::the_v2_next_methods_are_not_registered_yet` |
| Every new method answers `-32601` | nothing dispatches them | `crates/ainb-hangar-daemon/tests/v2_contract_dark.rs` |
| Peer listener off unless `AINB_HANGAR_PEER_LISTEN` is set at boot | `crates/ainb-hangar-daemon/src/peer_listener.rs` | `v2_contract_dark` |
| Terminal streams off unless `AINB_TERMINAL_STREAM=1` at boot (plus the `terminal-stream` feature, added by R2) | `crates/ainb-hangar-daemon/src/term/mod.rs` | `v2_contract_dark` |
| Phase switches are env only, never `daemon_config` keys | `DAEMON_CONFIG_REGISTRY` | `v2_contract_dark` |

## Types (RECONCILED 1.1)

| Row | Item | File |
|---|---|---|
| T1 | `HostId`: 26-char Crockford ULID or `"local"`; `parse_minted` refuses `local` | `crates/ainb-hangar-proto/src/hosts.rs` |
| T2 | `SessionRef { host_id, session_key }`; `session_key` is `fleet_session.session_key` | `crates/ainb-hangar-proto/src/session_ref.rs` |
| T3 | `CarrierKind`: `tailnet`, `lan`, `ssh-l` | `hosts.rs` |
| T4 | `Reachability`, `HostCoverage`, all `*_ms` | `hosts.rs` |
| T5 | `DeviceScope { base, admin }`; admin implies base desktop (constructor and decoder); `Grantor::may_grant`: the operator grants any scope, a device only `mobile` or `mobile+type` and never above its own (DV5, S1) | `crates/ainb-hangar-proto/src/devices.rs` |
| T6 | `DeviceRow` | `devices.rs` |
| T7 | `PairingOffer`: `ainb://pair#` + base64url(JSON), `expires_at_ms` | `crates/ainb-hangar-noise/src/offer.rs` |
| T8 | 16-byte LE header (magic 0x74), opcode registry (20 to 23 in use; 1 to 6, 8, 10, 11, 13 reserved; 7 and 9 retired), seven-field prologue | `crates/ainb-hangar-noise/src/{frame,opcode,prologue}.rs`, golden `tests/fixtures/peer_v1.json` |
| T9 | Close codes 1013, 4401, 4403, 4409, 4429, 4503; only 1013, 4429 and 4503 retry | `crates/ainb-hangar-proto/src/peer_close.rs` |
| T13 | `SurfaceKind::Mobile`; a local (unix-leg) hello that claims it is recorded as `unknown` | `crates/ainb-hangar-proto/src/connections.rs`, `crates/ainb-hangar-daemon/src/rpc/auth.rs` |
| T14 | `HelloResult.scope`, `device_expires_at_ms` | `crates/ainb-hangar-proto/src/auth.rs` |
| T15 | `terminal/frame { stream_id, seq, frame }`, nine frame kinds, base64 data | `crates/ainb-hangar-proto/src/terminal.rs` |
| T16 | seq, epoch and snapshot rules (module doc) | `terminal.rs` |
| T17 | `FloorHolder { principal, label, stream_id }`, floor keyed per stream | `terminal.rs` |

T10 (`Caller::Device`), T11 (`answered_by`) and T12 (`message_send.actor`) are
daemon behaviour owned by R1-04; their rules are recorded in the `devices.rs`
module doc.

## Methods (RECONCILED 1.2)

| Row | Method | Params and result |
|---|---|---|
| M1 | `device/redeem` | `DeviceRedeemParams`, `DeviceRedeemResult` (its `host_id` refuses `local`) |
| M2 | `device/invite_create` (operator only) | `DeviceInviteCreateParams`, `DeviceInviteCreateResult` |
| M3 | `device/list` | `DeviceListResult` |
| M4 | `device/revoke`, `device/rescope` | `DeviceRevokeParams`, `DeviceRescopeParams`, envelope flattened |
| M5 | `terminal/attach` | `TerminalAttachParams`, `TerminalAttachResult` |
| M6 | `terminal/detach`, `terminal/scrollback` | `TerminalDetachParams`, `TerminalScrollback{Params,Result}` |
| M7 | `terminal/ack` | `TerminalAckParams`, cumulative `consumed` |
| M8 | `terminal/input` | `TerminalInputParams`, envelope flattened |
| M9 | `terminal/floor` | `TerminalFloorParams`, `FloorState` |
| M10 | `terminal/resize` | `TerminalResizeParams`, `TerminalResizeResult` |
| M11 | floor refusal | `MUTATION_REJECTED (-32008)` with `REASON_FLOOR_DENIED`, `FloorDeniedData` |
| M13 | disabled answer | `METHOD_NOT_FOUND (-32601)` |

Names are in `crates/ainb-hangar-proto/src/methods.rs`, appended to
`ALL_METHODS`. `hangar/issue_create` and `hangar/issue_run`, dispatched by
the daemon but missing from the registry before the freeze, are appended too. `terminal/frame` is a notification, so it sits in
`TERMINAL_NOTIFICATION_METHODS` and not in `ALL_METHODS`, the same split the
fleet notifications use.

## Capabilities and scopes (RECONCILED 1.3)

| Row | Item | File |
|---|---|---|
| C1 | `hangar.peer.ws`, `hangar.peer.pair`, `hangar.peer.heartbeat`, `hangar.devices`, `hangar.scopes`, `hangar.session_ref` | `protocol.rs` |
| C2 | `terminal.stream`, `terminal.input` | `protocol.rs` |
| C3 | Per-scope method and event table, explicit verdict per method per scope, NO default, enforced from source: every method const in `methods.rs` and every method the daemon dispatches must have a row | `devices.rs` (`SCOPE_TABLE`, `EVENT_TABLE`), committed `crates/ainb-hangar-proto/scopes.catalogue`, `scope_catalogue`, `v2_contract_dark` |
| C4 | Lattice: mobile ⊆ mobile+type ⊆ desktop ⊆ desktop+admin ⊆ operator, per method and params | `crates/ainb-hangar-proto/tests/scope_catalogue.rs` |
| C5 | `fleet/action` interrupt only for a phone, on the typed `ControlAction` | `devices.rs` (`CallParams`, `Verdict::InterruptOnly`) |
| C6 | Mobile event families: Fleet, Attention, Transcript. Connections and Devices are admin only, and so are their read methods `hangar/connections_list` and `device/list` | `EVENT_TABLE` |
| C7 | Heartbeat numbers: ping 15 s, 2 missed Pongs, host closes after 35 s silence | `crates/ainb-hangar-noise/src/lib.rs` |
| C10, C11 | Env-only dark switches | `peer_listener.rs`, `term/mod.rs` |
| C13 | `ainb-hangar-noise` is a workspace member; `crates/ainb-wire-mobile` is excluded | root `Cargo.toml` |

## Forward compatibility

Every JSON wire enum added here decodes a value a newer build adds as
`Unknown` instead of refusing the message: `CarrierKind`, `Reachability`,
`HostCoverage`, `BaseScope`, `EventFamily`, `FloorAction`, `DataGapReason`,
`TerminalFrame`, `TerminalResizeResult`. An unknown base scope is granted
nothing; an unknown carrier is never written into a prologue. Close codes and
opcodes are numbers on the wire, decoded by `PeerClose::from_code` and
`Opcode::from_u8`, which answer `None` for a number this build does not know.

## Gates in CI

- Contracts: `wire_contract`, `scope_catalogue`, `v2_contract_dark`, beside the
  existing contract binaries. Never rerun one to green.
- Lint: `cargo check -p ainb-hangar-noise` for `aarch64-apple-ios`,
  `aarch64-apple-ios-sim`, `x86_64-apple-ios`, `aarch64-linux-android` and
  `x86_64-linux-android`.

Regenerate after an intended change:

- scope table: `UPDATE_SCOPE_CATALOGUE=1 cargo test -p ainb-hangar-proto --test scope_catalogue`
- peer wire: `UPDATE_PEER_GOLDEN=1 cargo test -p ainb-hangar-noise --test wire_contract`

## Pre-declared modules

Empty files so each lane owns its file and never edits a shared one:
`crates/ainb-hangar-client/src/{terminal,peer,pairings}.rs` and
`crates/ainb-hangar-daemon/src/{term/mod.rs,peer_listener.rs,host_key.rs}`.
The `crates/ainb-term` skeleton is owned by R2 WP0 (its own PR), not here.
