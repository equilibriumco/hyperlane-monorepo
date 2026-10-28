use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use hyperlane_core::{
    Announcement, ContractLocator, HyperlaneChain, HyperlaneDomain, HyperlaneMessage,
    HyperlaneProvider, KnownHyperlaneDomain, Mailbox, Metadata, ReorgPeriod, Signable, Signature,
    SignedType, ValidatorAnnounce, H160, H256, U256,
};
use tempfile::TempDir;
use url::Url;

use crate::{ConnectionConf, MidnightMailbox, MidnightValidatorAnnounce};

struct StubSubmitter {
    _dir: TempDir,
    binary: PathBuf,
    request_log: PathBuf,
}

impl StubSubmitter {
    fn always_returns(response_json: &str) -> Self {
        Self::with_reply(&format!(
            "cat <<'__SUBMITTER_RESPONSE__'\n{response_json}\n__SUBMITTER_RESPONSE__\n"
        ))
    }

    /// Reads the request and exits without printing anything.
    fn silent() -> Self {
        Self::with_reply("")
    }

    fn with_reply(reply_script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("submit");
        let request_log = dir.path().join("request.json");

        let script = format!(
            "#!/bin/sh\ncat > '{log}'\n{reply_script}",
            log = request_log.display(),
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

    /// Whether the agent ran the stub at all: it logs every request it gets.
    fn was_called(&self) -> bool {
        self.request_log.exists()
    }

    fn captured_request(&self) -> serde_json::Value {
        let raw = std::fs::read_to_string(&self.request_log).expect("stub captured no stdin");
        serde_json::from_str(&raw).expect("captured stdin is not valid JSON")
    }
}

const TEST_CONTRACT_ADDRESS: H256 = H256::repeat_byte(0xA1);

fn test_conf(toolkit_path: Option<String>) -> ConnectionConf {
    ConnectionConf::new(
        Url::parse("http://127.0.0.1:8088/api/v3/graphql").unwrap(),
        toolkit_path,
    )
}

fn mailbox_with_conf(conf: &ConnectionConf) -> MidnightMailbox {
    let domain = HyperlaneDomain::Known(KnownHyperlaneDomain::Midnight);
    let locator = ContractLocator {
        domain: &domain,
        address: TEST_CONTRACT_ADDRESS,
    };
    MidnightMailbox::new(&locator, conf)
        .unwrap()
        .with_validator_override(vec![])
}

fn build_mailbox(stub: &StubSubmitter) -> MidnightMailbox {
    mailbox_with_conf(&test_conf(Some(stub.binary_path().to_owned())))
}

fn validator_announce_with_conf(conf: &ConnectionConf) -> MidnightValidatorAnnounce {
    let domain = HyperlaneDomain::Known(KnownHyperlaneDomain::Midnight);
    let locator = ContractLocator {
        domain: &domain,
        address: TEST_CONTRACT_ADDRESS,
    };
    MidnightValidatorAnnounce::new(&locator, conf).unwrap()
}

/// An announcement the relayer-side checks accept, so the call reaches the
/// sidecar: a padded location and a signature that recovers to some key.
fn sample_announcement() -> SignedType<Announcement> {
    use ethers::signers::LocalWallet;

    let mut location = b"s3://hyperlane-validator-0/us-east-1".to_vec();
    location.resize(crate::toolkit::MAX_STORAGE_LOCATION_LEN, 0);
    let announcement = Announcement {
        validator: H160::repeat_byte(0x11),
        mailbox_address: H256::repeat_byte(0xab),
        mailbox_domain: 1234,
        storage_location: String::from_utf8(location).unwrap(),
    };
    let wallet: LocalWallet = "1111111111111111111111111111111111111111111111111111111111111111"
        .parse()
        .unwrap();
    let digest = announcement.eth_signed_message_hash();
    let sig = <[u8; 65]>::from(wallet.sign_hash(ethers::types::H256(digest.0)));
    SignedType {
        value: announcement,
        signature: Signature {
            r: U256::from_big_endian(&sig[0..32]),
            s: U256::from_big_endian(&sig[32..64]),
            v: sig[64] as u64,
        },
    }
}

const ERROR_KINDS: [&str; 9] = [
    "proofTimeout",
    "insufficientDust",
    "staleState",
    "replay",
    "signerUnavailable",
    "rpcUnreachable",
    "nodeRejected",
    "contractRevert",
    "internal",
];

fn error_reply(kind: &str) -> String {
    format!(r#"{{"error":{{"kind":"{kind}","message":"reported by the stub"}}}}"#)
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
    // `false` would make the relayer drop every inbound message at its
    // `is_recipient_contract` gate, before `process` ever runs.
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
    // Forwarded unpadded; the submitter pads the on-chain `Vector<4>`.
    assert_eq!(sigs.len(), 2);
    assert_eq!(
        sigs[0].as_str().unwrap(),
        format!("0x{}", hex::encode([0x11; 65]))
    );
    assert_eq!(
        sigs[1].as_str().unwrap(),
        format!("0x{}", hex::encode([0x22; 65]))
    );
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
    assert!(req["indexerGraphqlUrl"]
        .as_str()
        .unwrap()
        .contains("graphql"));
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
    assert_eq!(&raw[60..], &[0xde, 0xad, 0xbe, 0xef]);
    assert!(raw[..60].iter().all(|b| *b == 0));
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
        mailbox
            .recipient_ism(H256::repeat_byte(0xEE))
            .await
            .unwrap(),
        TEST_CONTRACT_ADDRESS
    );
}

#[tokio::test]
async fn count_returns_zero_for_destination_only_impl() {
    let stub = StubSubmitter::always_returns("{}");
    let mailbox = build_mailbox(&stub);
    assert_eq!(mailbox.count(&ReorgPeriod::None).await.unwrap(), 0);
}

// The request must be the `dryRunHandle` op, or the estimate is succeeding
// blindly rather than simulating.
#[tokio::test]
async fn process_estimate_costs_dry_runs_and_returns_placeholder_on_accept() {
    let stub = StubSubmitter::always_returns(r#"{"ok":true}"#);
    let mailbox = build_mailbox(&stub);
    let estimate = mailbox
        .process_estimate_costs(&sample_message(), &sample_metadata())
        .await
        .unwrap();
    assert!(estimate.gas_limit.is_zero());

    let req = stub.captured_request();
    assert_eq!(req["op"], "dryRunHandle");
    assert_eq!(req["contractAddress"], format!("{TEST_CONTRACT_ADDRESS:x}"));
    assert_eq!(req["isContractRecipient"], false);
    // No transaction is built, so the proof endpoint is absent.
    assert!(req.get("proofServerUrl").is_none());
}

// A detected revert has to error here, so the relayer maps it to
// `ErrorEstimatingGas` and backs off instead of busy-looping at submit time.
#[tokio::test]
async fn process_estimate_costs_errs_when_dry_run_detects_revert() {
    let stub = StubSubmitter::always_returns(
        r#"{"error":{"kind":"contractRevert","message":"Routes: sender not enrolled"}}"#,
    );
    let mailbox = build_mailbox(&stub);
    let err = mailbox
        .process_estimate_costs(&sample_message(), &sample_metadata())
        .await
        .unwrap_err();
    let display = format!("{err}");
    assert!(
        display.contains("contractRevert"),
        "unexpected error: {display}"
    );
    assert!(
        display.contains("not enrolled"),
        "unexpected error: {display}"
    );
}

// Unparseable metadata fails before the dry run, so a structurally broken
// message backs off too rather than reaching submit.
#[tokio::test]
async fn process_estimate_costs_errs_on_unparseable_metadata() {
    let stub = StubSubmitter::always_returns(r#"{"ok":true}"#);
    let mailbox = build_mailbox(&stub);
    let short = Metadata::new(vec![0u8; 10]);
    let result = mailbox
        .process_estimate_costs(&sample_message(), &short)
        .await;
    assert!(result.is_err(), "expected a metadata parse error");
    // The relayer's own parser refused it; the sidecar was never asked.
    assert!(!stub.was_called(), "the stub sidecar received a request");
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

// Every kind the sidecar can report ends in an error from the submit and from
// the dry run alike. That is the one retry path the relayer has: a failed dry
// run re-prepares the message as ErrorEstimatingGas and a failed submit as
// ErrorSubmitting, and neither looks at the kind, which survives only as text.
#[tokio::test]
async fn every_error_kind_errs_both_submit_and_dry_run() {
    for kind in ERROR_KINDS {
        let stub = StubSubmitter::always_returns(&error_reply(kind));
        let mailbox = build_mailbox(&stub);

        let err = mailbox
            .process(&sample_message(), &sample_metadata(), None)
            .await
            .expect_err(&format!("submit returned ok for kind {kind}"));
        assert!(format!("{err}").contains(kind), "kind {kind} lost: {err}");

        let err = mailbox
            .process_estimate_costs(&sample_message(), &sample_metadata())
            .await
            .expect_err(&format!("dry run returned ok for kind {kind}"));
        assert!(format!("{err}").contains(kind), "kind {kind} lost: {err}");
    }
}

// A reply the agent cannot read is an error too, never a success or a crash.
#[tokio::test]
async fn every_malformed_reply_errs_both_submit_and_dry_run() {
    let stubs = [
        ("nothing printed", StubSubmitter::silent()),
        (
            "text that is not JSON",
            StubSubmitter::always_returns("not even json"),
        ),
        (
            "neither a result nor an error",
            StubSubmitter::always_returns("{}"),
        ),
    ];
    for (what, stub) in &stubs {
        let mailbox = build_mailbox(stub);

        let err = mailbox
            .process(&sample_message(), &sample_metadata(), None)
            .await
            .expect_err(&format!("submit returned ok on {what}"));
        let display = format!("{err}").to_lowercase();
        assert!(
            display.contains("malformed") || display.contains("missing"),
            "{what}: unexpected submit error: {display}"
        );

        let err = mailbox
            .process_estimate_costs(&sample_message(), &sample_metadata())
            .await
            .expect_err(&format!("dry run returned ok on {what}"));
        let display = format!("{err}").to_lowercase();
        assert!(
            display.contains("malformed") || display.contains("missing"),
            "{what}: unexpected dry run error: {display}"
        );
    }
}

#[tokio::test]
async fn fewer_location_lists_than_validators_is_malformed() {
    // Lists map to validators by position, so a short reply has to error
    // rather than misalign.
    let stub = StubSubmitter::always_returns(r#"{"locations":[["s3://only-one"]]}"#);
    let va = validator_announce_with_conf(&test_conf(Some(stub.binary_path().to_owned())));
    let err = va
        .get_announced_storage_locations(&[H256::repeat_byte(0x11), H256::repeat_byte(0x22)])
        .await
        .unwrap_err();
    let display = format!("{err}");
    assert!(
        display.contains("expected 2 location lists, got 1"),
        "unexpected error: {display}"
    );
}

// A failed delivered check is an error carrying the kind, never a verdict.
#[tokio::test]
async fn delivered_errs_on_every_error_kind_and_malformed_reply() {
    for kind in ERROR_KINDS {
        let stub = StubSubmitter::always_returns(&error_reply(kind));
        let mailbox = build_mailbox(&stub);
        let err = mailbox
            .delivered(H256::repeat_byte(0xCC))
            .await
            .expect_err(&format!("delivered returned a verdict for kind {kind}"));
        assert!(format!("{err}").contains(kind), "kind {kind} lost: {err}");
    }
    let stubs = [
        ("nothing printed", StubSubmitter::silent()),
        (
            "text that is not JSON",
            StubSubmitter::always_returns("not even json"),
        ),
        ("no delivered field", StubSubmitter::always_returns("{}")),
    ];
    for (what, stub) in &stubs {
        let mailbox = build_mailbox(stub);
        let err = mailbox
            .delivered(H256::repeat_byte(0xCC))
            .await
            .expect_err(&format!("delivered returned a verdict on {what}"));
        let display = format!("{err}").to_lowercase();
        assert!(
            display.contains("malformed") || display.contains("missing"),
            "{what}: unexpected error: {display}"
        );
    }
}

// With no sidecar path configured every call fails before anything is
// spawned, with an error that names the setting to fix.
#[tokio::test]
async fn missing_sidecar_path_fails_every_call_at_once() {
    let conf = test_conf(None);
    let mailbox = mailbox_with_conf(&conf);
    let va = validator_announce_with_conf(&conf);
    let started = Instant::now();

    let errors = [
        (
            "submit",
            mailbox
                .process(&sample_message(), &sample_metadata(), None)
                .await
                .map(|_| ())
                .unwrap_err(),
        ),
        (
            "dry run",
            mailbox
                .process_estimate_costs(&sample_message(), &sample_metadata())
                .await
                .map(|_| ())
                .unwrap_err(),
        ),
        (
            "delivered",
            mailbox
                .delivered(H256::zero())
                .await
                .map(|_| ())
                .unwrap_err(),
        ),
        (
            "storage locations",
            va.get_announced_storage_locations(&[H256::repeat_byte(0x11)])
                .await
                .map(|_| ())
                .unwrap_err(),
        ),
        (
            "announce",
            va.announce(sample_announcement())
                .await
                .map(|_| ())
                .unwrap_err(),
        ),
    ];
    assert!(started.elapsed() < Duration::from_secs(1));
    for (what, err) in errors {
        let display = format!("{err}");
        assert!(
            display.contains("toolkitPath"),
            "{what}: error does not name the setting: {display}"
        );
    }
}

/// Live check: a hostile message through the relayer's own request builder
/// into the real sidecar, which must refuse it with nothing reaching the
/// chain. Needs the devnet with `night` deployed, and the relayer seed in the
/// environment because the sidecar reads it from there. The sidecar checks the
/// body before it opens the wallet, so the body cases cost no wallet sync.
/// Run with:
///
///   MIDNIGHT_SIDECAR_PATH=<hyperlane-midnight>/relayer/bin/submit-handle \
///   MIDNIGHT_NIGHT_ADDRESS=<deployed night address hex> \
///   MIDNIGHT_RELAYER_SEED=<relayer seed> \
///   cargo test --release -p hyperlane-midnight hostile_message -- --ignored --nocapture
#[tokio::test]
#[ignore = "requires a running Midnight devnet with night deployed and the real sidecar"]
async fn hostile_message_through_the_real_sidecar_lands_nothing() {
    use ethers::signers::LocalWallet;

    let sidecar = std::env::var("MIDNIGHT_SIDECAR_PATH")
        .expect("set MIDNIGHT_SIDECAR_PATH to relayer/bin/submit-handle");
    let address_hex = std::env::var("MIDNIGHT_NIGHT_ADDRESS")
        .expect("set MIDNIGHT_NIGHT_ADDRESS to the deployed night contract address (hex)");
    std::env::var("MIDNIGHT_RELAYER_SEED").expect("set MIDNIGHT_RELAYER_SEED for the sidecar");
    let indexer_url = std::env::var("MIDNIGHT_INDEXER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8088/api/v3/graphql".to_string());
    let indexer = Url::parse(&indexer_url).expect("valid indexer URL");

    let night = H256::from_slice(&hex::decode(address_hex.trim_start_matches("0x")).unwrap());
    let domain = HyperlaneDomain::Known(KnownHyperlaneDomain::Midnight);
    let conf = ConnectionConf::new(indexer.clone(), Some(sidecar));
    let deployed = MidnightMailbox::new(
        &ContractLocator {
            domain: &domain,
            address: night,
        },
        &conf,
    )
    .unwrap()
    .with_validator_override(vec![]);
    let undeployed = MidnightMailbox::new(
        &ContractLocator {
            domain: &domain,
            address: H256::repeat_byte(0xD0),
        },
        &conf,
    )
    .unwrap()
    .with_validator_override(vec![]);

    // The sidecar recovers a public key from each signature before the circuit
    // sees the message, so the signature has to be a real one; what it signs
    // does not matter, the body is refused first.
    let wallet: LocalWallet = "1111111111111111111111111111111111111111111111111111111111111111"
        .parse()
        .unwrap();
    let sig = <[u8; 65]>::from(wallet.sign_hash(ethers::types::H256([0x42u8; 32])));
    let mut metadata = Vec::with_capacity(32 + 32 + 4 + 65);
    metadata.extend_from_slice(&[0xCD; 32]);
    metadata.extend_from_slice(&[0xEE; 32]);
    metadata.extend_from_slice(&7u32.to_be_bytes());
    metadata.extend_from_slice(&sig);
    let metadata = Metadata::new(metadata);
    let message = |body: Vec<u8>| HyperlaneMessage {
        version: 3,
        nonce: 0,
        origin: 5,
        sender: H256::repeat_byte(0xAB),
        destination: 1234,
        recipient: night,
        body,
    };

    let tip_before = block_height(&indexer).await;

    // The token message is 64 bytes: one byte short, and a body as large as an
    // origin chain would carry (the Hyperlane contracts cap nothing).
    for (what, body) in [
        ("a 63-byte body", vec![0x11u8; 63]),
        ("a 128 KiB body", vec![0x11u8; 2 * 2usize.pow(16)]),
    ] {
        let started = Instant::now();
        let err = deployed
            .process(&message(body), &metadata, None)
            .await
            .expect_err(&format!("{what} was accepted"));
        let display = format!("{err}");
        println!("{what}: refused after {:?}: {display}", started.elapsed());
        assert!(
            display.contains("submitter reported error: internal"),
            "{what}: expected the sidecar's internal kind: {display}"
        );
    }

    let err = undeployed
        .process_estimate_costs(&message(vec![0u8; 64]), &metadata)
        .await
        .expect_err("a dry run against an undeployed address was accepted");
    let display = format!("{err}");
    println!("undeployed contract: {display}");
    assert!(
        display.contains("submitter reported error: contractRevert"),
        "expected the sidecar's contractRevert kind: {display}"
    );

    let landed = transactions_since(&indexer, tip_before).await;
    assert!(
        landed.is_empty(),
        "transactions landed on the devnet during the run: {landed:?}"
    );
}

async fn graphql(indexer: &Url, query: &str) -> serde_json::Value {
    let reply: serde_json::Value = reqwest::Client::new()
        .post(indexer.clone())
        .json(&serde_json::json!({ "query": query }))
        .send()
        .await
        .expect("indexer reachable")
        .json()
        .await
        .expect("indexer JSON");
    assert!(reply.get("errors").is_none(), "indexer errors: {reply}");
    reply["data"].clone()
}

async fn block_height(indexer: &Url) -> u64 {
    graphql(indexer, "{ block { height } }").await["block"]["height"]
        .as_u64()
        .expect("a block height")
}

/// Hashes of every transaction in the blocks after `from_height`, with the
/// height each landed in.
async fn transactions_since(indexer: &Url, from_height: u64) -> Vec<(u64, String)> {
    let tip = block_height(indexer).await;
    let mut found = Vec::new();
    for height in from_height + 1..=tip {
        let data = graphql(
            indexer,
            &format!("{{ block(offset: {{ height: {height} }}) {{ transactions {{ hash }} }} }}"),
        )
        .await;
        if let Some(transactions) = data["block"]["transactions"].as_array() {
            for tx in transactions {
                found.push((height, tx["hash"].as_str().unwrap_or("").to_string()));
            }
        }
    }
    found
}
