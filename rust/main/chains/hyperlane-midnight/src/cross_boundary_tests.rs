use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use hyperlane_core::{
    ContractLocator, HyperlaneChain, HyperlaneDomain, HyperlaneMessage, HyperlaneProvider,
    KnownHyperlaneDomain, Mailbox, Metadata, ReorgPeriod, H256,
};
use tempfile::TempDir;
use url::Url;

use crate::{ConnectionConf, MidnightMailbox};

struct StubSubmitter {
    _dir: TempDir,
    binary: PathBuf,
    request_log: PathBuf,
}

impl StubSubmitter {
    fn always_returns(response_json: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("submit");
        let request_log = dir.path().join("request.json");

        let script = format!(
            "#!/bin/sh\ncat > '{log}'\ncat <<'__SUBMITTER_RESPONSE__'\n{resp}\n__SUBMITTER_RESPONSE__\n",
            log = request_log.display(),
            resp = response_json,
        );
        std::fs::write(&binary, script).unwrap();
        let mut perms = std::fs::metadata(&binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&binary, perms).unwrap();

        Self {
            _dir: dir,
            binary,
            request_log,
        }
    }

    fn binary_path(&self) -> &str {
        self.binary.to_str().unwrap()
    }

    fn captured_request(&self) -> serde_json::Value {
        let raw = std::fs::read_to_string(&self.request_log).expect("stub captured no stdin");
        serde_json::from_str(&raw).expect("captured stdin is not valid JSON")
    }
}

const TEST_CONTRACT_ADDRESS: H256 = H256::repeat_byte(0xA1);

fn build_mailbox(stub: &StubSubmitter) -> MidnightMailbox {
    let domain = HyperlaneDomain::Known(KnownHyperlaneDomain::Midnight);
    let locator = ContractLocator {
        domain: &domain,
        address: TEST_CONTRACT_ADDRESS,
    };
    let conf = ConnectionConf::new(
        Url::parse("http://127.0.0.1:8088/api/v3/graphql").unwrap(),
        Some(stub.binary_path().to_owned()),
    );
    MidnightMailbox::new(&locator, &conf).unwrap()
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

fn sample_metadata() -> Metadata {
    let mut bytes = Vec::with_capacity(32 + 32 + 4 + 2 * 65);
    bytes.extend_from_slice(&[0xCD; 32]);
    bytes.extend_from_slice(&[0xEE; 32]);
    bytes.extend_from_slice(&7u32.to_be_bytes());
    bytes.extend_from_slice(&[0x11; 65]);
    bytes.extend_from_slice(&[0x22; 65]);
    Metadata::new(bytes)
}

#[tokio::test]
async fn delivered_resolves_true_response_to_true() {
    let stub = StubSubmitter::always_returns(r#"{"delivered":true}"#);
    let mailbox = build_mailbox(&stub);
    assert!(mailbox.delivered(H256::repeat_byte(0xCC)).await.unwrap());
}

#[tokio::test]
async fn delivered_resolves_false_response_to_false() {
    let stub = StubSubmitter::always_returns(r#"{"delivered":false}"#);
    let mailbox = build_mailbox(&stub);
    assert!(!mailbox.delivered(H256::repeat_byte(0xCC)).await.unwrap());
}

#[tokio::test]
async fn delivered_propagates_submitter_error_as_structured_kind() {
    let stub = StubSubmitter::always_returns(
        r#"{"error":{"kind":"rpcUnreachable","message":"econnrefused"}}"#,
    );
    let mailbox = build_mailbox(&stub);
    let err = mailbox.delivered(H256::zero()).await.unwrap_err();
    assert!(format!("{err}").contains("rpcUnreachable"));
}

#[tokio::test]
async fn is_contract_returns_true_for_monolithic_warp_route() {
    // Returning false makes `pending_message` drop every inbound message
    // at the `is_recipient_contract` gate before `Mailbox::process` runs.
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    let is_contract = mailbox
        .provider()
        .is_contract(&H256::repeat_byte(0x77))
        .await
        .unwrap();
    assert!(is_contract);
}

#[tokio::test]
async fn process_request_json_pins_wire_shape() {
    let stub = StubSubmitter::always_returns(r#"{"txHash":"0x42","blockHeight":1}"#);
    let mailbox = build_mailbox(&stub);

    mailbox
        .process(&sample_message(), &sample_metadata(), None)
        .await
        .unwrap();

    let req = stub.captured_request();
    assert_eq!(req["op"], "submit");
    assert_eq!(req["contractAddress"], format!("{TEST_CONTRACT_ADDRESS:x}"));
    assert_eq!(req["isContractRecipient"], false);

    let msg = &req["message"];
    assert_eq!(msg["version"], 3);
    assert_eq!(msg["nonce"], "0");
    assert_eq!(msg["origin"], 5);
    assert_eq!(msg["destination"], 1234);
    assert!(msg["sender"].as_str().unwrap().starts_with("0x"));
    assert!(msg["recipient"].as_str().unwrap().starts_with("0x"));
    assert!(msg["body"].as_str().unwrap().starts_with("0x"));

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

    let sigs = md["signatures"].as_array().unwrap();
    assert_eq!(sigs.len(), 16);
    assert_eq!(sigs[0].as_str().unwrap(), format!("0x{}", hex::encode([0x11; 65])));
    assert_eq!(sigs[1].as_str().unwrap(), format!("0x{}", hex::encode([0x22; 65])));
    assert_eq!(sigs[2].as_str().unwrap(), format!("0x{}", hex::encode([0u8; 65])));
}

#[tokio::test]
async fn delivered_request_json_pins_wire_shape() {
    let stub = StubSubmitter::always_returns(r#"{"delivered":false}"#);
    let mailbox = build_mailbox(&stub);

    let msg_id = H256::repeat_byte(0xAA);
    mailbox.delivered(msg_id).await.unwrap();

    let req = stub.captured_request();
    assert_eq!(req["op"], "isDelivered");
    assert_eq!(req["contractAddress"], format!("{TEST_CONTRACT_ADDRESS:x}"));
    assert_eq!(req["messageId"], format!("0x{msg_id:x}"));
    assert!(req["indexerGraphqlUrl"].as_str().unwrap().contains("graphql"));
}

#[tokio::test]
async fn process_returns_tx_outcome_on_success() {
    let stub = StubSubmitter::always_returns(r#"{"txHash":"0xdeadbeef","blockHeight":12345}"#);
    let mailbox = build_mailbox(&stub);

    let outcome = mailbox
        .process(&sample_message(), &sample_metadata(), None)
        .await
        .unwrap();
    assert!(outcome.executed);
    let raw = outcome.transaction_id.as_bytes();
    assert_eq!(&raw[..4], &[0xde, 0xad, 0xbe, 0xef]);
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
        .unwrap_err();
    assert!(format!("{err}").contains("replay"));
}

#[tokio::test]
async fn process_maps_contract_revert_response_to_submitter_reported() {
    let stub = StubSubmitter::always_returns(
        r#"{"error":{"kind":"contractRevert","message":"failed assert: Routes: domain not enrolled"}}"#,
    );
    let mailbox = build_mailbox(&stub);
    let err = mailbox
        .process(&sample_message(), &sample_metadata(), None)
        .await
        .unwrap_err();
    let display = format!("{err}");
    assert!(display.contains("contractRevert"));
    assert!(display.contains("Routes: domain not enrolled"));
}

#[tokio::test]
async fn process_rejects_malformed_response_as_malformed() {
    let stub = StubSubmitter::always_returns("not even json");
    let mailbox = build_mailbox(&stub);
    let err = mailbox
        .process(&sample_message(), &sample_metadata(), None)
        .await
        .unwrap_err();
    let display = format!("{err}").to_lowercase();
    assert!(display.contains("malformed") || display.contains("expected"));
}

#[tokio::test]
async fn default_ism_returns_contract_address() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    assert_eq!(
        Mailbox::default_ism(&mailbox).await.unwrap(),
        TEST_CONTRACT_ADDRESS
    );
}

#[tokio::test]
async fn recipient_ism_returns_contract_address_for_any_recipient() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    assert_eq!(
        mailbox.recipient_ism(H256::repeat_byte(0xEE)).await.unwrap(),
        TEST_CONTRACT_ADDRESS
    );
}

#[tokio::test]
async fn count_returns_zero_for_destination_only_impl() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    assert_eq!(mailbox.count(&ReorgPeriod::None).await.unwrap(), 0);
}

#[tokio::test]
async fn process_estimate_costs_returns_non_zero_placeholder() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    let estimate = mailbox
        .process_estimate_costs(&sample_message(), &sample_metadata())
        .await
        .unwrap();
    assert!(!estimate.gas_limit.is_zero());
}

#[tokio::test]
async fn process_calldata_is_unimplemented_lander_only() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    let err = mailbox
        .process_calldata(&sample_message(), &sample_metadata())
        .await
        .unwrap_err();
    let display = format!("{err}").to_lowercase();
    assert!(display.contains("not implemented") || display.contains("lander"));
}

#[tokio::test]
async fn delivered_calldata_returns_none_lander_only() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    assert!(mailbox.delivered_calldata(H256::zero()).unwrap().is_none());
}

#[tokio::test]
async fn stub_captures_full_payload_for_inspection() {
    let stub = StubSubmitter::always_returns(r#"{"delivered":true}"#);
    let mailbox = build_mailbox(&stub);
    mailbox.delivered(H256::repeat_byte(0xBB)).await.unwrap();
    let req = stub.captured_request();
    assert!(req.is_object());
    assert_eq!(req["op"], "isDelivered");
}
