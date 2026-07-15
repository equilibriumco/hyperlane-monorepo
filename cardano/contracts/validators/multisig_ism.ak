use cardano/transaction.{OutputReference, Transaction}
use multisig.{
  validate_ism_continuation, validate_set_threshold, validate_set_validators,
  verify_checkpoint,
}
use types.{
  Checkpoint, MessageIdMultisig, MigrateIsm, MultisigIsmDatum,
  MultisigIsmRedeemer, SetThreshold, SetValidators, Verify,
}
use utils.{validate_migrate}

/// MessageId Multisig ISM (Interchain Security Module) Validator
/// Verifies message authenticity using N-of-M validator signatures.
///
/// This is the "MessageId" flavour: the delivered message is the one named
/// directly by the signed checkpoint (`checkpoint.message_id`). The mailbox
/// binds delivery by checking that field against the message being processed.
/// (The MerkleRoot flavour lives in `merkleroot_ism.ak` and binds via a proof.)
///
/// Signature verification (shared, see lib/multisig.ak):
/// 1. Relayer recovers public keys off-chain and passes compressed + uncompressed
/// 2. ISM checks the two formats share an x-coordinate, then verifies the sig
/// 3. ISM derives the Ethereum address and checks it is a trusted validator
/// 4. Unique valid signatures must meet the per-origin threshold (> 0)
validator multisig_ism {
  spend(
    datum: Option<MultisigIsmDatum>,
    redeemer: MultisigIsmRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(ism_datum) = datum

    when redeemer is {
      Verify { checkpoint, validator_signatures } -> {
        expect verify_checkpoint(ism_datum, checkpoint, validator_signatures)
        validate_ism_continuation(ism_datum, ism_datum, tx, own_ref)
      }

      SetValidators { domain, validators } ->
        validate_set_validators(ism_datum, domain, validators, tx, own_ref)

      SetThreshold { domain, threshold } ->
        validate_set_threshold(ism_datum, domain, threshold, tx, own_ref)

      MigrateIsm { new_address } -> {
        let datum_as_data: Data = ism_datum
        validate_migrate(ism_datum.owner, new_address, tx, own_ref, datum_as_data)
      }
    }
  }

  else(_) {
    fail
  }
}

test verify_checkpoint_rejects_zero_threshold() fail {
  let origin = 1
  let datum =
    MultisigIsmDatum {
      validators: [(origin, [])],
      thresholds: [(origin, 0)],
      owner: #"00000000000000000000000000000000000000000000000000000000",
      module_type: MessageIdMultisig,
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
