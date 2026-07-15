use aiken/collection/list
use cardano/transaction.{OutputReference, Transaction}
use merkle
use multisig.{
  validate_ism_continuation, validate_set_threshold, validate_set_validators,
  verify_checkpoint,
}
use types.{
  Checkpoint, MerkleRootIsmRedeemer, MerkleRootVerify, MrMigrateIsm,
  MrSetThreshold, MrSetValidators, MultisigIsmDatum,
}
use utils.{validate_migrate}

/// MerkleRoot Multisig ISM Validator
///
/// The "MerkleRoot" flavour. Unlike the MessageId ISM, the signed checkpoint is
/// at some index `j` and may not name the delivered message directly. The
/// delivered message is instead proven to be a leaf under the signed root:
///
///   1. A threshold of validators signed `checkpoint.merkle_root` (shared
///      verify_checkpoint — same signature/threshold logic as the MessageId ISM).
///   2. `delivered_message_id` is the leaf at `delivered_index` under that root
///      (merkle inclusion proof).
///
/// The mailbox binds delivery by checking `delivered_message_id` against the
/// message being processed (and that the proof depth / index are well-formed).
/// This lets an older message be validated by any later signed root, which is
/// the whole point of MerkleRoot: resilience to per-index checkpoint gaps.
validator merkleroot_ism {
  spend(
    datum: Option<MultisigIsmDatum>,
    redeemer: MerkleRootIsmRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(ism_datum) = datum

    when redeemer is {
      MerkleRootVerify {
        checkpoint,
        validator_signatures,
        delivered_message_id,
        delivered_index,
        merkle_proof,
      } -> {
        // Fixed proof depth (matches the incremental tree depth).
        expect list.length(merkle_proof) == 32
        // A leaf cannot sit beyond the signed frontier.
        expect delivered_index <= checkpoint.merkle_index
        // Threshold of validators actually signed this root.
        expect verify_checkpoint(ism_datum, checkpoint, validator_signatures)
        // The delivered message is provably a leaf under that signed root.
        expect
          merkle.verify_proof(
            checkpoint.merkle_root,
            delivered_message_id,
            delivered_index,
            merkle_proof,
          )
        validate_ism_continuation(ism_datum, ism_datum, tx, own_ref)
      }

      MrSetValidators { domain, validators } ->
        validate_set_validators(ism_datum, domain, validators, tx, own_ref)

      MrSetThreshold { domain, threshold } ->
        validate_set_threshold(ism_datum, domain, threshold, tx, own_ref)

      MrMigrateIsm { new_address } -> {
        let datum_as_data: Data = ism_datum
        validate_migrate(ism_datum.owner, new_address, tx, own_ref, datum_as_data)
      }
    }
  }

  else(_) {
    fail
  }
}

// A MerkleRootVerify against a zero-threshold domain must be rejected before any
// signature or proof is trusted (mirrors the MessageId ISM's guard). Full
// happy-path coverage needs real validator signatures + a real inclusion proof
// and is exercised end-to-end; the proof math itself is covered in merkle_test.
test merkleroot_verify_rejects_zero_threshold() fail {
  let origin = 1
  let datum =
    MultisigIsmDatum {
      validators: [(origin, [])],
      thresholds: [(origin, 0)],
      owner: #"00000000000000000000000000000000000000000000000000000000",
    }
  let checkpoint =
    Checkpoint {
      origin,
      merkle_root: #"0000000000000000000000000000000000000000000000000000000000000000",
      origin_merkle_tree_hook: #"0000000000000000000000000000000000000000000000000000000000000000",
      merkle_index: 0,
      message_id: #"0000000000000000000000000000000000000000000000000000000000000000",
    }
  verify_checkpoint(datum, checkpoint, [])
}
