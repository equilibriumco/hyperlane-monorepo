use aiken/collection/dict
use aiken/collection/list
use aiken/collection/pairs
use cardano/address.{Script}
use cardano/assets
use cardano/transaction.{Spend, Transaction}

/// Minimal redeemer type used only to verify that the recipient script was
/// invoked with the protocol-standard Init redeemer (constructor index 0).
type InitRedeemer {
  Init
}

/// Canonical config NFT minting policy.
///
/// Authenticates the ISM-override config UTXO at a recipient's script address.
/// This is a fixed (non-parameterized) policy — its ID is a protocol constant.
///
/// The asset name of the minted token IS the recipient's script hash (28 bytes),
/// so the policy ID is sufficient for the relayer to derive the config token for
/// any `0x02 + script_hash` recipient without per-recipient pre-configuration.
///
/// Minting is only allowed when a spent input at `Script(asset_name)` carries a
/// constructor-0 redeemer (the protocol-standard `Init` tag). This ensures only
/// the legitimate contract owner can mint the config token — an attacker who
/// lacks the script's `owner` key cannot produce a valid `Init` redeemer.
validator canonical_config_nft {
  mint(_redeemer: Data, policy_id: ByteArray, tx: Transaction) {
    let minted = assets.tokens(tx.mint, policy_id)

    // Exactly one token minted; its asset name is the recipient's script hash.
    let script_hash_bytes =
      when dict.keys(minted) is {
        [key] -> key
        _ -> fail
      }
    expect dict.get(minted, script_hash_bytes) == Some(1)

    // Require a spent input at the recipient's script address.
    expect Some(init_input) =
      list.find(
        tx.inputs,
        fn(input) {
          when input.output.address.payment_credential is {
            Script(h) -> h == script_hash_bytes
            _ -> False
          }
        },
      )

    // That input's redeemer must be constructor 0 (the protocol Init tag).
    expect Some(raw_redeemer) =
      pairs.get_first(tx.redeemers, Spend(init_input.output_reference))
    expect _: InitRedeemer = raw_redeemer

    True
  }

  else(_) {
    fail
  }
}
