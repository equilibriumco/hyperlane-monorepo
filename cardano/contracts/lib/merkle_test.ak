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
