use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray

pub const empty_leaf =
  #"bc36789e7a1e281436464229828f817d6612f7b477d66591ff96a9e064bcc98a"

pub const present_leaf =
  #"5fe7f977e71dba2ea1a68e21057beebb9be2ac30c6410aa38d4f3fbe41dcffd2"

pub const empty_root =
  #"5c3cc358c060877ced35947091c44c900594ece1e0a4ade23143ef57c3f7600f"

fn get_bit(key: ByteArray, index: Int) -> Int {
  let byte_val = bytearray.at(key, index / 8)
  let bit_pos = 7 - index % 8
  byte_val / pow2(bit_pos) % 2
}

fn pow2(n: Int) -> Int {
  when n is {
    0 -> 1
    1 -> 2
    2 -> 4
    3 -> 8
    4 -> 16
    5 -> 32
    6 -> 64
    7 -> 128
    _ -> 1
  }
}

/// Verify a key is NOT in the tree (non-membership), then return the new root
/// with the key inserted. Fails if the key is already present.
/// key = first 16 bytes of message_id. proof = 128 sibling hashes (leaf→root).
pub fn verify_non_membership_and_insert(
  root: ByteArray,
  key: ByteArray,
  proof: List<ByteArray>,
) -> ByteArray {
  expect list.length(proof) == 128
  let (computed_old_root, new_root, _) =
    list.foldl(
      proof,
      (empty_leaf, present_leaf, 0),
      fn(sibling, acc) {
        let (old_cur, new_cur, idx) = acc
        let bit = get_bit(key, idx)
        if bit == 0 {
          (
            keccak_256(bytearray.concat(old_cur, sibling)),
            keccak_256(bytearray.concat(new_cur, sibling)),
            idx + 1,
          )
        } else {
          (
            keccak_256(bytearray.concat(sibling, old_cur)),
            keccak_256(bytearray.concat(sibling, new_cur)),
            idx + 1,
          )
        }
      },
    )
  expect computed_old_root == root
  new_root
}
