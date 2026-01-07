use aiken/builtin.{verify_ecdsa_secp256k1_signature}
use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use cardano/transaction.{InlineDatum, Output, OutputReference, Transaction}
use types.{
  Domain, PolicyId, ValidatorAnnounceDatum, ValidatorAnnounceRedeemer,
  Announce, Revoke,
}

/// Validator Announce Contract
/// Allows validators to publicly announce their checkpoint storage locations
///
/// This enables relayers to discover where validators store their signed checkpoints.
/// Each validator creates a UTXO at this script address with their announcement datum.
///
/// **Security Model:**
/// Unlike Cardano-native signature verification (ed25519), this contract uses
/// ECDSA secp256k1 signatures - the same cryptography used by Ethereum and Hyperlane.
/// This ensures:
/// 1. Validator identity is consistent across all chains (Ethereum address)
/// 2. ISMs on any chain can verify checkpoints using the same validator set
/// 3. Cross-chain interoperability is maintained
///
/// **Announcement Flow:**
/// 1. Validator has a secp256k1 keypair (same key used for signing checkpoints)
/// 2. Validator computes announcement digest: keccak256(domain_hash || storage_location)
/// 3. Validator signs the digest with their secp256k1 key (EIP-191 format)
/// 4. Transaction submitter provides signature + public key in redeemer
/// 5. Contract verifies signature and derives Ethereum address from public key
/// 6. Announcement is stored with the Ethereum address as validator identity
///
validator validator_announce(mailbox_policy_id: PolicyId, mailbox_domain: Domain) {
  spend(
    datum: Option<ValidatorAnnounceDatum>,
    redeemer: ValidatorAnnounceRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    when redeemer is {
      Announce { storage_location, compressed_pubkey, uncompressed_pubkey, signature } ->
        validate_announce(
          datum,
          mailbox_policy_id,
          mailbox_domain,
          storage_location,
          compressed_pubkey,
          uncompressed_pubkey,
          signature,
          tx,
          own_ref,
        )

      Revoke { compressed_pubkey, uncompressed_pubkey, signature } ->
        validate_revoke(datum, mailbox_policy_id, mailbox_domain, compressed_pubkey, uncompressed_pubkey, signature)
    }
  }

  else(_) {
    fail
  }
}

/// Validate a new announcement or update to existing
fn validate_announce(
  datum: Option<ValidatorAnnounceDatum>,
  mailbox_policy_id: PolicyId,
  mailbox_domain: Domain,
  new_storage_location: ByteArray,
  compressed_pubkey: ByteArray,
  uncompressed_pubkey: ByteArray,
  signature: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Storage location must not be empty
  expect bytearray.length(new_storage_location) > 0

  // Verify public key formats match (same x-coordinate)
  expect verify_pubkey_formats(compressed_pubkey, uncompressed_pubkey)

  // Compute the announcement digest that should have been signed
  let announcement_digest = compute_announcement_digest(
    mailbox_policy_id,
    mailbox_domain,
    new_storage_location,
  )

  // Verify the ECDSA signature
  expect verify_ecdsa_secp256k1_signature(
    compressed_pubkey,
    announcement_digest,
    signature,
  )

  // Derive Ethereum address from uncompressed public key
  let validator_address = pubkey_to_eth_address(uncompressed_pubkey)

  when datum is {
    // New announcement - validator is creating their first announcement
    None -> validate_new_announcement(
      mailbox_policy_id,
      mailbox_domain,
      new_storage_location,
      validator_address,
      tx,
      own_ref,
    )

    // Update existing announcement
    Some(existing_datum) -> validate_update_announcement(
      existing_datum,
      new_storage_location,
      validator_address,
      tx,
      own_ref,
    )
  }
}

/// Validate creating a new announcement
fn validate_new_announcement(
  mailbox_policy_id: PolicyId,
  mailbox_domain: Domain,
  storage_location: ByteArray,
  validator_address: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Find own input to get script address
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address

  // Find continuation output at same address
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // Parse the new datum
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: ValidatorAnnounceDatum = cont_datum_data

  // Verify the datum is properly formed
  expect bytearray.length(cont_datum.validator_address) == 20
  expect cont_datum.validator_address == validator_address
  expect cont_datum.mailbox_policy_id == mailbox_policy_id
  expect cont_datum.mailbox_domain == mailbox_domain
  expect cont_datum.storage_location == storage_location

  True
}

/// Validate updating an existing announcement
fn validate_update_announcement(
  existing_datum: ValidatorAnnounceDatum,
  new_storage_location: ByteArray,
  validator_address: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Only the validator who created the announcement can update it
  // This is verified by checking the signature-derived address matches
  expect validator_address == existing_datum.validator_address

  // Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address

  // Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // Parse continuation datum
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: ValidatorAnnounceDatum = cont_datum_data

  // Verify only storage_location changed, other fields preserved
  expect cont_datum.validator_address == existing_datum.validator_address
  expect cont_datum.mailbox_policy_id == existing_datum.mailbox_policy_id
  expect cont_datum.mailbox_domain == existing_datum.mailbox_domain
  expect cont_datum.storage_location == new_storage_location

  True
}

/// Validate revoking an announcement
fn validate_revoke(
  datum: Option<ValidatorAnnounceDatum>,
  mailbox_policy_id: PolicyId,
  mailbox_domain: Domain,
  compressed_pubkey: ByteArray,
  uncompressed_pubkey: ByteArray,
  signature: ByteArray,
) -> Bool {
  // Can only revoke an existing announcement
  expect Some(existing_datum) = datum

  // Verify public key formats match
  expect verify_pubkey_formats(compressed_pubkey, uncompressed_pubkey)

  // Compute revocation digest (use empty storage location to indicate revocation)
  let revocation_digest = compute_announcement_digest(
    mailbox_policy_id,
    mailbox_domain,
    #"",  // Empty storage location indicates revocation
  )

  // Verify the ECDSA signature
  expect verify_ecdsa_secp256k1_signature(
    compressed_pubkey,
    revocation_digest,
    signature,
  )

  // Derive Ethereum address and verify it matches the announcement
  let validator_address = pubkey_to_eth_address(uncompressed_pubkey)
  expect validator_address == existing_datum.validator_address

  // No continuation output required - the UTXO is consumed
  True
}

/// Compute the announcement digest that validators sign
/// This matches the Hyperlane EVM ValidatorAnnounce format:
/// 1. domain_hash = keccak256(localDomain || mailboxAddress || "HYPERLANE_ANNOUNCEMENT")
/// 2. inner_hash = keccak256(domain_hash || storage_location)
/// 3. announcement_digest = keccak256("\x19Ethereum Signed Message:\n32" || inner_hash)
fn compute_announcement_digest(
  mailbox_policy_id: PolicyId,
  mailbox_domain: Domain,
  storage_location: ByteArray,
) -> ByteArray {
  // Step 1: domain_hash
  // Encode domain as 4-byte big-endian
  let domain_bytes = encode_u32_be(mailbox_domain)
  // Mailbox address as 32 bytes (policy ID padded with zeros on right)
  let mailbox_address = bytearray.concat(mailbox_policy_id, #"00000000")
  // "HYPERLANE_ANNOUNCEMENT" as bytes
  let announcement_string = #"48595045524c414e455f414e4e4f554e43454d454e54"

  let domain_hash_input = bytearray.concat(domain_bytes, bytearray.concat(mailbox_address, announcement_string))
  let domain_hash = keccak_256(domain_hash_input)

  // Step 2: inner_hash = keccak256(domain_hash || storage_location)
  let inner_hash_input = bytearray.concat(domain_hash, storage_location)
  let inner_hash = keccak_256(inner_hash_input)

  // Step 3: EIP-191 signed message hash
  // Prefix: "\x19Ethereum Signed Message:\n32"
  let eip191_prefix = #"19457468657265756d205369676e6564204d6573736167653a0a3332"
  let prefixed = bytearray.concat(eip191_prefix, inner_hash)
  keccak_256(prefixed)
}

/// Encode a u32 as 4-byte big-endian
fn encode_u32_be(n: Int) -> ByteArray {
  let b0 = n / 16777216 % 256
  let b1 = n / 65536 % 256
  let b2 = n / 256 % 256
  let b3 = n % 256
  bytearray.from_int_big_endian(b0, 1)
    |> bytearray.concat(bytearray.from_int_big_endian(b1, 1))
    |> bytearray.concat(bytearray.from_int_big_endian(b2, 1))
    |> bytearray.concat(bytearray.from_int_big_endian(b3, 1))
}

/// Verify that compressed and uncompressed public key formats match
/// The x-coordinate should be the same: compressed[1:33] == uncompressed[0:32]
fn verify_pubkey_formats(compressed: ByteArray, uncompressed: ByteArray) -> Bool {
  expect bytearray.length(compressed) == 33
  expect bytearray.length(uncompressed) == 64

  let compressed_x = bytearray.drop(compressed, 1)
  let uncompressed_x = bytearray.take(uncompressed, 32)
  compressed_x == uncompressed_x
}

/// Convert an uncompressed secp256k1 public key (64 bytes) to Ethereum address (20 bytes)
/// address = keccak256(pubkey)[12:32] (last 20 bytes)
fn pubkey_to_eth_address(pubkey: ByteArray) -> ByteArray {
  expect bytearray.length(pubkey) == 64
  let hash = keccak_256(pubkey)
  bytearray.drop(hash, 12)
}

/// Find an input by output reference
fn find_input(tx: Transaction, ref: OutputReference) {
  list.find(
    tx.inputs,
    fn(input) { input.output_reference == ref },
  )
}
