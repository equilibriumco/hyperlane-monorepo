use aiken/collection/list
use aiken/crypto.{keccak_256}
use merkle

/// Test that an empty tree has count 0
test empty_tree_has_zero_count() {
  let tree = merkle.empty()
  tree.count == 0
}

/// Test that an empty tree has 32 zero-hash branches (max_depth)
test empty_tree_has_initialized_branches() {
  let tree = merkle.empty()
  // Tree initializes with max_depth (32) zero hashes
  list.length(tree.branches) == 32
}

/// Test inserting into empty tree increments count
test insert_increments_count() {
  let tree = merkle.empty()
  let leaf = keccak_256("test message")
  let new_tree = merkle.insert(tree, leaf)
  new_tree.count == 1
}

/// Test multiple inserts increment count correctly
test multiple_inserts_increment_count() {
  let tree = merkle.empty()
  let leaf1 = keccak_256("message 1")
  let leaf2 = keccak_256("message 2")
  let leaf3 = keccak_256("message 3")

  let tree1 = merkle.insert(tree, leaf1)
  let tree2 = merkle.insert(tree1, leaf2)
  let tree3 = merkle.insert(tree2, leaf3)

  tree3.count == 3
}

/// Test that root changes after insert
test root_changes_after_insert() {
  let tree = merkle.empty()
  let initial_root = merkle.root(tree)

  let leaf = keccak_256("test message")
  let new_tree = merkle.insert(tree, leaf)
  let new_root = merkle.root(new_tree)

  initial_root != new_root
}

/// Test that different messages produce different roots
test different_messages_different_roots() {
  let tree1 = merkle.empty()
  let tree2 = merkle.empty()

  let leaf1 = keccak_256("message A")
  let leaf2 = keccak_256("message B")

  let tree1_with_leaf = merkle.insert(tree1, leaf1)
  let tree2_with_leaf = merkle.insert(tree2, leaf2)

  merkle.root(tree1_with_leaf) != merkle.root(tree2_with_leaf)
}

/// Test that same message produces same root
test same_message_same_root() {
  let tree1 = merkle.empty()
  let tree2 = merkle.empty()

  let message = "same message"
  let leaf = keccak_256(message)

  let tree1_with_leaf = merkle.insert(tree1, leaf)
  let tree2_with_leaf = merkle.insert(tree2, leaf)

  merkle.root(tree1_with_leaf) == merkle.root(tree2_with_leaf)
}

/// Test hash_pair produces consistent results
test hash_pair_consistent() {
  let a = keccak_256("left")
  let b = keccak_256("right")

  let hash1 = merkle.hash_pair(a, b)
  let hash2 = merkle.hash_pair(a, b)

  hash1 == hash2
}

/// Test hash_pair is order-dependent
test hash_pair_order_dependent() {
  let a = keccak_256("left")
  let b = keccak_256("right")

  let hash1 = merkle.hash_pair(a, b)
  let hash2 = merkle.hash_pair(b, a)

  hash1 != hash2
}

// ============================================================================
// Round-trip consistency: insert/root vs verify_proof
//
// A MerkleRoot ISM is only sound if a proof built for a leaf verifies against
// the SAME root the tree produces. These tests exercise that invariant (O5).
// ============================================================================

/// zero-subtree hash at a given level (level 0 = the empty-leaf zero hash)
fn zero_at(level: Int) -> ByteArray {
  if level <= 0 {
    merkle.zero_hash
  } else {
    let child = zero_at(level - 1)
    merkle.hash_pair(child, child)
  }
}

/// [zero_at(from), zero_at(from+1), ..., zero_at(max_depth-1)]
fn zeros_from(from: Int) -> List<ByteArray> {
  if from >= 32 {
    []
  } else {
    [zero_at(from), ..zeros_from(from + 1)]
  }
}

/// A single-leaf tree: the leaf's siblings are all zero subtrees.
test proof_roundtrips_single_leaf() {
  let leaf = keccak_256("only message")
  let tree = merkle.insert(merkle.empty(), leaf)
  // sibling at every level is the zero subtree of that level
  let proof = zeros_from(0)
  merkle.verify_proof(merkle.root(tree), leaf, 0, proof)
}

/// A two-leaf tree: leaf 0's level-0 sibling is leaf 1, everything above is zero.
test proof_roundtrips_two_leaves_index0() {
  let l0 = keccak_256("message 0")
  let l1 = keccak_256("message 1")
  let tree = merkle.insert(merkle.insert(merkle.empty(), l0), l1)
  let proof = [l1, ..zeros_from(1)]
  merkle.verify_proof(merkle.root(tree), l0, 0, proof)
}

/// Same two-leaf tree, proving leaf 1 (index bit 0 set → leaf is the right child).
test proof_roundtrips_two_leaves_index1() {
  let l0 = keccak_256("message 0")
  let l1 = keccak_256("message 1")
  let tree = merkle.insert(merkle.insert(merkle.empty(), l0), l1)
  let proof = [l0, ..zeros_from(1)]
  merkle.verify_proof(merkle.root(tree), l1, 1, proof)
}

/// A wrong sibling must NOT verify (guards against a trivially-passing verifier).
test proof_rejects_wrong_sibling() {
  let l0 = keccak_256("message 0")
  let l1 = keccak_256("message 1")
  let tree = merkle.insert(merkle.insert(merkle.empty(), l0), l1)
  let bad_proof = [keccak_256("not the sibling"), ..zeros_from(1)]
  !merkle.verify_proof(merkle.root(tree), l0, 0, bad_proof)
}

/// Fold a node up to the tree root hashing against zero subtrees (standard).
fn fold_zeros_up(node: ByteArray, from: Int) -> ByteArray {
  if from >= 32 {
    node
  } else {
    fold_zeros_up(merkle.hash_pair(node, zero_at(from)), from + 1)
  }
}

/// Independent check that verify_proof reproduces the *standard* Hyperlane root
/// (computed here by hand, not via merkle.root). Establishes verify_proof as the
/// canonical function, so the two-leaf round-trip failures are root()'s fault.
test verify_proof_matches_standard_root() {
  let l0 = keccak_256("message 0")
  let l1 = keccak_256("message 1")
  let std_root = fold_zeros_up(merkle.hash_pair(l0, l1), 1)
  let proof = [l1, ..zeros_from(1)]
  merkle.verify_proof(std_root, l0, 0, proof)
}
