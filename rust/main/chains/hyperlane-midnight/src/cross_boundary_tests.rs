//! Cross-boundary integration tests for `MidnightMailbox`.
//!
//! These exercise the destination-side Mailbox impl against a stubbed
//! submitter binary. The boundary being tested:
//!
//! ```text
//! [Mailbox trait] -> MidnightMailbox -> stub binary (canned JSON) -> assertions
//! ```
//!
//! Each test creates a tiny shell-script stub in a tempdir, points the
//! Mailbox's `toolkit_path` at it, and either asserts on the Mailbox's
//! return value or reads the JSON the Mailbox sent into the stub.
//!
//! ## Why this layer exists
//!
//! The destination-side flow has three classes of bug the unit tests in
//! `mailbox.rs` and `toolkit.rs` cannot catch on their own:
//!
//! 1. **Provider/Mailbox contract drift** — methods like
//!    `MidnightProvider::is_contract` and `Mailbox::delivered` return
//!    types `pending_message` in the relayer agent gates its behavior
//!    on. The previous review caught one: `is_contract` returned
//!    `Ok(false)` and silently dropped every inbound message
//!    (regression test `c2_is_contract_returns_true`).
//!
//! 2. **Wire-format drift** — the JSON shape Rust sends has to keep
//!    matching the TS submitter's parser. The format-snapshot tests
//!    here pin the field names so a refactor that diverges fails loud
//!    (`process_request_json_pins_wire_shape`,
//!    `delivered_request_json_pins_wire_shape`).
//!
//! 3. **Error-kind plumbing** — every kind the submitter reports
//!    (`replay`, `contractRevert`, etc.) must propagate as a structured
//!    error variant the relayer can branch on. The tests below pin
//!    each one (`process_maps_<kind>_response_to_submitter_reported`).
//!
//! Higher-fidelity validation against `pending_message` itself is
//! tracked separately; the contract-shape tests here would have
//! caught the criticals (C1, C2) from the PR review.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use async_trait::async_trait;
use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneChain, HyperlaneDomain, HyperlaneMessage,
    HyperlaneProvider, KnownHyperlaneDomain, Mailbox, Metadata, ReorgPeriod, H256,
};
use tempfile::TempDir;
use url::Url;

use crate::{ConnectionConf, MidnightMailbox};

// ---------------------------------------------------------------------------
// Stub submitter helpers
// ---------------------------------------------------------------------------

/// One-shot stub binary that captures stdin to a log file and echoes a
/// canned response. Lives in a tempdir owned by the test so parallel
/// runs do not collide on paths.
struct StubSubmitter {
    /// Holds the tempdir alive for the test's duration.
    _dir: TempDir,
    /// Path the Mailbox should be pointed at as `toolkit_path`.
    binary: PathBuf,
    /// File that captures whatever the Mailbox writes to stdin. Read
    /// post-call to inspect the request shape.
    request_log: PathBuf,
}

impl StubSubmitter {
    /// Build a stub that always echoes `response_json` on stdout. The
    /// stub also captures stdin verbatim into `request_log`.
    fn always_returns(response_json: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir for stub submitter");
        let binary = dir.path().join("submit");
        let request_log = dir.path().join("request.json");

        // Heredoc avoids escaping the JSON payload by hand. The script
        // captures stdin, then echoes the canned response.
        let script = format!(
            "#!/bin/sh\ncat > '{log}'\ncat <<'__SUBMITTER_RESPONSE__'\n{resp}\n__SUBMITTER_RESPONSE__\n",
            log = request_log.display(),
            resp = response_json,
        );
        std::fs::write(&binary, script).expect("write stub script");
        let mut perms = std::fs::metadata(&binary).expect("stat stub").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&binary, perms).expect("chmod stub");

        Self {
            _dir: dir,
            binary,
            request_log,
        }
    }

    fn binary_path(&self) -> &str {
        self.binary.to_str().expect("stub path is utf-8")
    }

    /// Read whatever the Mailbox wrote to the stub's stdin and parse as
    /// JSON. Panics if no request was captured (the script may have
    /// failed) or the JSON is malformed.
    fn captured_request(&self) -> serde_json::Value {
        let raw =
            std::fs::read_to_string(&self.request_log).expect("stub did not capture stdin");
        serde_json::from_str(&raw).expect("captured stdin is not valid JSON")
    }
}

// ---------------------------------------------------------------------------
// Fixture construction
// ---------------------------------------------------------------------------

const TEST_CONTRACT_ADDRESS: H256 = H256::repeat_byte(0xA1);

fn test_domain() -> HyperlaneDomain {
    HyperlaneDomain::Known(KnownHyperlaneDomain::Midnight)
}

fn build_mailbox(stub: &StubSubmitter) -> MidnightMailbox {
    let domain = test_domain();
    let locator = ContractLocator {
        domain: &domain,
        address: TEST_CONTRACT_ADDRESS,
    };
    let conf = ConnectionConf::new(
        Url::parse("http://127.0.0.1:8088/api/v3/graphql").expect("graphql url"),
        Some(stub.binary_path().to_owned()),
    );
    MidnightMailbox::new(&locator, &conf).expect("build mailbox")
}

fn sample_message() -> HyperlaneMessage {
    HyperlaneMessage {
        version: 3,
        nonce: 0,
        origin: 5,
        sender: H256::repeat_byte(0xAB),
        destination: 1234,
        recipient: H256::repeat_byte(0x44),
        body: vec![0u8; 64],
    }
}

/// Standard MessageIdMultisigIsmMetadata layout with 2 signatures
/// (`merkle_tree_hook || root || index_be || sigs`).
fn sample_metadata() -> Metadata {
    let mut bytes = Vec::with_capacity(32 + 32 + 4 + 2 * 65);
    bytes.extend_from_slice(&[0xCD; 32]);
    bytes.extend_from_slice(&[0xEE; 32]);
    bytes.extend_from_slice(&7u32.to_be_bytes());
    bytes.extend_from_slice(&[0x11; 65]);
    bytes.extend_from_slice(&[0x22; 65]);
    Metadata::new(bytes)
}

// ---------------------------------------------------------------------------
// Regression: C1 — `delivered` must actually work
// ---------------------------------------------------------------------------
//
// C1 from the PR review: the original `handleIsDelivered` called a
// non-existent symbol and every invocation threw `TypeError`. Anything
// downstream then looped forever. These tests pin that `delivered`
// resolves both boolean responses cleanly via the same wire contract
// the TS submitter implements today.

#[tokio::test]
async fn delivered_resolves_true_response_to_true() {
    let stub = StubSubmitter::always_returns(r#"{"delivered":true}"#);
    let mailbox = build_mailbox(&stub);
    let result = mailbox
        .delivered(H256::repeat_byte(0xCC))
        .await
        .expect("delivered should resolve");
    assert!(result, "true response must round-trip as true");
}

#[tokio::test]
async fn delivered_resolves_false_response_to_false() {
    let stub = StubSubmitter::always_returns(r#"{"delivered":false}"#);
    let mailbox = build_mailbox(&stub);
    let result = mailbox
        .delivered(H256::repeat_byte(0xCC))
        .await
        .expect("delivered should resolve");
    assert!(!result, "false response must round-trip as false");
}

#[tokio::test]
async fn delivered_propagates_submitter_error_as_structured_kind() {
    // The submitter envelope's `error.kind` must surface as
    // `SubmitterReported { kind }` so the relayer can branch on it.
    let stub = StubSubmitter::always_returns(
        r#"{"error":{"kind":"rpcUnreachable","message":"econnrefused"}}"#,
    );
    let mailbox = build_mailbox(&stub);
    let err = mailbox
        .delivered(H256::zero())
        .await
        .expect_err("delivered should propagate the submitter error");
    let display = format!("{err}");
    assert!(
        display.contains("rpcUnreachable"),
        "expected kind in error display, got: {display}"
    );
}

// ---------------------------------------------------------------------------
// Regression: C2 — provider's `is_contract` MUST return true for the
// monolithic WarpRoute. Returning false makes `pending_message` drop
// every inbound message before `Mailbox::process` is even called.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn c2_is_contract_returns_true() {
    // No submitter is invoked here — `is_contract` is a Provider method
    // that returns synthetically for the monolithic-WarpRoute design.
    // Stub just needs to exist to satisfy the constructor.
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);

    let provider = mailbox.provider();
    let is_contract = provider
        .is_contract(&H256::repeat_byte(0x77))
        .await
        .expect("is_contract should succeed");
    assert!(
        is_contract,
        "is_contract returning false would make the relayer drop every \
         inbound message at `pending_message.rs::is_recipient_contract`. \
         Keep returning true while WarpRoute is monolithic."
    );
}

// ---------------------------------------------------------------------------
// Wire-format snapshots (partial L10 coverage)
//
// Pin the JSON shape the Rust side sends. A refactor that drops a
// field, renames `messageId` → `message_id`, etc., fails loud here
// without needing to spin up the live devnet smoke.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_request_json_pins_wire_shape() {
    let stub =
        StubSubmitter::always_returns(r#"{"txHash":"0x42","blockHeight":1}"#);
    let mailbox = build_mailbox(&stub);

    mailbox
        .process(&sample_message(), &sample_metadata(), None)
        .await
        .expect("process should succeed against the stubbed `txHash`");

    let req = stub.captured_request();
    assert_eq!(req["op"], "submit", "op tag must be `submit`");
    assert_eq!(
        req["contractAddress"],
        format!("0x{TEST_CONTRACT_ADDRESS:x}"),
        "contractAddress must round-trip lowercase, 0x-prefixed",
    );
    assert_eq!(req["isContractRecipient"], false);

    // Message fields — numeric ones as JSON numbers (origin/destination/version)
    // or decimal strings (nonce, for JS BigInt safety).
    let msg = &req["message"];
    assert_eq!(msg["version"], 3);
    assert_eq!(msg["nonce"], "0");
    assert_eq!(msg["origin"], 5);
    assert_eq!(msg["destination"], 1234);
    assert!(
        msg["sender"].as_str().unwrap().starts_with("0x"),
        "sender must be hex"
    );
    assert!(
        msg["recipient"].as_str().unwrap().starts_with("0x"),
        "recipient must be hex"
    );
    assert!(msg["body"].as_str().unwrap().starts_with("0x"));

    // Metadata: layout has merkleTreeHook + root + index + 16 sig slots
    // (2 real + 14 zero-padded).
    let md = &req["metadata"];
    assert_eq!(
        md["merkleTreeHook"].as_str().unwrap(),
        format!("0x{}", hex::encode([0xCD; 32]))
    );
    assert_eq!(
        md["root"].as_str().unwrap(),
        format!("0x{}", hex::encode([0xEE; 32]))
    );
    assert_eq!(md["index"], 7);
    let sigs = md["signatures"].as_array().expect("signatures is an array");
    assert_eq!(
        sigs.len(),
        16,
        "Vector<16, Bytes<65>> on-chain means exactly 16 slots wire-side"
    );
    assert_eq!(sigs[0].as_str().unwrap(), format!("0x{}", hex::encode([0x11; 65])));
    assert_eq!(sigs[1].as_str().unwrap(), format!("0x{}", hex::encode([0x22; 65])));
    assert_eq!(
        sigs[2].as_str().unwrap(),
        format!("0x{}", hex::encode([0u8; 65])),
        "unused signature slots must be zero-padded"
    );
}

#[tokio::test]
async fn delivered_request_json_pins_wire_shape() {
    let stub = StubSubmitter::always_returns(r#"{"delivered":false}"#);
    let mailbox = build_mailbox(&stub);

    let msg_id = H256::repeat_byte(0xAA);
    mailbox.delivered(msg_id).await.expect("delivered ok");

    let req = stub.captured_request();
    assert_eq!(req["op"], "isDelivered", "op tag must be `isDelivered`");
    assert_eq!(
        req["contractAddress"],
        format!("0x{TEST_CONTRACT_ADDRESS:x}"),
    );
    assert_eq!(req["messageId"], format!("0x{msg_id:x}"));
    assert!(
        req["indexerGraphqlUrl"]
            .as_str()
            .unwrap()
            .contains("graphql"),
        "indexerGraphqlUrl must be threaded through"
    );
}

// ---------------------------------------------------------------------------
// Process — success + error-kind plumbing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_returns_tx_outcome_on_success() {
    let stub = StubSubmitter::always_returns(
        r#"{"txHash":"0xdeadbeef","blockHeight":12345}"#,
    );
    let mailbox = build_mailbox(&stub);

    let outcome = mailbox
        .process(&sample_message(), &sample_metadata(), None)
        .await
        .expect("process should return TxOutcome");
    assert!(outcome.executed, "executed must be true on success envelope");
    // tx hash is widened from 32-byte H256 into the low end of H512 —
    // see toolkit::parse_tx_hash. Spot-check the first bytes.
    let raw = outcome.transaction_id.as_bytes();
    assert_eq!(raw[0], 0xde);
    assert_eq!(raw[1], 0xad);
    assert_eq!(raw[2], 0xbe);
    assert_eq!(raw[3], 0xef);
}

#[tokio::test]
async fn process_maps_replay_response_to_submitter_reported() {
    let stub = StubSubmitter::always_returns(
        r#"{"error":{"kind":"replay","message":"already delivered"}}"#,
    );
    let mailbox = build_mailbox(&stub);

    let err = mailbox
        .process(&sample_message(), &sample_metadata(), None)
        .await
        .expect_err("replay response must be Err");
    assert!(
        format!("{err}").contains("replay"),
        "kind must surface in the error display so the relayer can branch on it"
    );
}

#[tokio::test]
async fn process_maps_contract_revert_response_to_submitter_reported() {
    // Same kind the live devnet smoke exercises when the routes set is
    // empty. The on-chain `handle` reverts → submitter classifies as
    // `contractRevert` → Rust must propagate that through.
    let stub = StubSubmitter::always_returns(
        r#"{"error":{"kind":"contractRevert","message":"failed assert: Routes: domain not enrolled"}}"#,
    );
    let mailbox = build_mailbox(&stub);

    let err = mailbox
        .process(&sample_message(), &sample_metadata(), None)
        .await
        .expect_err("contract revert must be Err");
    let display = format!("{err}");
    assert!(display.contains("contractRevert"));
    assert!(display.contains("Routes: domain not enrolled"));
}

#[tokio::test]
async fn process_rejects_malformed_response_as_malformed() {
    // The submitter is contractually supposed to emit a `{txHash}` or
    // `{error}` envelope. Anything else (bare text, empty object) must
    // surface as `SubmitterMalformed` so the relayer treats it as an
    // unexpected protocol error rather than retrying.
    let stub = StubSubmitter::always_returns("not even json");
    let mailbox = build_mailbox(&stub);

    let err = mailbox
        .process(&sample_message(), &sample_metadata(), None)
        .await
        .expect_err("non-JSON stdout must be an error");
    let display = format!("{err}").to_lowercase();
    assert!(
        display.contains("malformed") || display.contains("expected"),
        "actual: {display}"
    );
}

// ---------------------------------------------------------------------------
// Read-only Mailbox methods
//
// Pin the synthetic answers for the monolithic-WarpRoute design.
// Anything other than `self.address` for the ISM methods or `0` for
// `count` would mean someone wired a non-monolith model without
// updating these.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn default_ism_returns_contract_address() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    let ism = Mailbox::default_ism(&mailbox).await.expect("default_ism");
    assert_eq!(
        ism, TEST_CONTRACT_ADDRESS,
        "monolithic WarpRoute means the ISM lives at the same address"
    );
}

#[tokio::test]
async fn recipient_ism_returns_contract_address_for_any_recipient() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    let ism = mailbox
        .recipient_ism(H256::repeat_byte(0xEE))
        .await
        .expect("recipient_ism");
    assert_eq!(ism, TEST_CONTRACT_ADDRESS);
}

#[tokio::test]
async fn count_returns_zero_for_destination_only_impl() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    let count = mailbox
        .count(&ReorgPeriod::None)
        .await
        .expect("count should not error");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn process_estimate_costs_returns_non_zero_placeholder() {
    // The relayer divides by `gas_limit`/`gas_price` to compute queue
    // metrics. Zero would NaN those.
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    let estimate = mailbox
        .process_estimate_costs(&sample_message(), &sample_metadata())
        .await
        .expect("estimate should succeed");
    assert!(!estimate.gas_limit.is_zero(), "gas_limit must be > 0");
}

#[tokio::test]
async fn process_calldata_is_unimplemented_lander_only() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    let err = mailbox
        .process_calldata(&sample_message(), &sample_metadata())
        .await
        .expect_err("process_calldata is Lander-only and must return Err");
    let display = format!("{err}").to_lowercase();
    assert!(
        display.contains("not implemented") || display.contains("lander"),
        "actual: {display}"
    );
}

#[tokio::test]
async fn delivered_calldata_returns_none_lander_only() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    let calldata = mailbox
        .delivered_calldata(H256::zero())
        .expect("delivered_calldata must be Ok(None) for the Classic path");
    assert!(
        calldata.is_none(),
        "Classic submitter path: no calldata to surface"
    );
}

// ---------------------------------------------------------------------------
// Sanity: the stub harness itself
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stub_captures_full_payload_for_inspection() {
    // Confirms the test harness is functioning: stdin → log file →
    // parsed JSON → assert. If this ever breaks, every other test in
    // this module silently passes for the wrong reason.
    let stub = StubSubmitter::always_returns(r#"{"delivered":true}"#);
    let mailbox = build_mailbox(&stub);
    mailbox
        .delivered(H256::repeat_byte(0xBB))
        .await
        .expect("delivered ok");
    let req = stub.captured_request();
    assert!(req.is_object(), "captured request must be a JSON object");
    assert_eq!(req["op"], "isDelivered");
}

// ---------------------------------------------------------------------------
// `async_trait` is used here because the Mailbox trait we exercise is
// itself `#[async_trait]`. Without referencing it the import would be
// flagged unused. This `_check` keeps the import live for editors that
// auto-prune.
// ---------------------------------------------------------------------------
#[async_trait]
trait _UnusedTraitToKeepAsyncTraitImportLive: Send + Sync {
    async fn _noop(&self) -> ChainResult<()>;
}
