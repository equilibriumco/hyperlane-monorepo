use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray

/// Merkle tree implementation for Hyperlane Cardano
/// Uses keccak-256 for hashing (Hyperlane/EVM compatible)
///
/// The merkle tree is an incremental tree that supports appending leaves.
/// It maintains a list of branch hashes at each level.
/// Zero hash for empty nodes (32 bytes of zeros)
pub const zero_hash: ByteArray =
  #"0000000000000000000000000000000000000000000000000000000000000000"

/// Maximum tree depth (supports 2^32 - 1 messages)
pub const max_depth: Int = 32

/// Merkle tree state
pub type MerkleTree {
  /// Branch hashes at each level (from leaf to root)
  branches: List<ByteArray>,
  /// Number of leaves inserted
  count: Int,
}

/// Create an empty merkle tree
pub fn empty() -> MerkleTree {
  MerkleTree { branches: initial_branches(max_depth), count: 0 }
}

/// Initialize branches with zero hashes
fn initial_branches(depth: Int) -> List<ByteArray> {
  if depth <= 0 {
    []
  } else {
    [zero_hash, ..initial_branches(depth - 1)]
  }
}

/// Insert a leaf into the merkle tree
/// Returns the new tree state
pub fn insert(tree: MerkleTree, leaf: ByteArray) -> MerkleTree {
  let new_branches = insert_leaf(tree.branches, leaf, tree.count, 0)
  MerkleTree { branches: new_branches, count: tree.count + 1 }
}

/// Recursive helper to insert a leaf
fn insert_leaf(
  branches: List<ByteArray>,
  node: ByteArray,
  index: Int,
  level: Int,
) -> List<ByteArray> {
  when branches is {
    [] -> []
    [branch, ..rest] -> {
      // Check if this level needs to be updated
      let bit = index / pow2(level) % 2
      if bit == 0 {
        // Left child: store the node as the new branch
        [node, ..rest]
      } else {
        // Right child: hash with the stored left sibling and continue up
        let parent = hash_pair(branch, node)
        [zero_hash, ..insert_leaf(rest, parent, index, level + 1)]
      }
    }
  }
}

/// Compute the root of the merkle tree
pub fn root(tree: MerkleTree) -> ByteArray {
  compute_root(tree.branches, tree.count, 0, zero_hash)
}

/// Recursive helper to compute root
fn compute_root(
  branches: List<ByteArray>,
  index: Int,
  level: Int,
  current: ByteArray,
) -> ByteArray {
  when branches is {
    [] -> current
    [branch, ..rest] -> {
      let bit = index / pow2(level) % 2
      let next =
        if bit == 0 {
          // No right sibling yet, hash with zero
          if level == 0 {
            branch
          } else {
            hash_pair(current, zero_hashes_at_level(level))
          }
        } else {
          // Has left sibling in branch
          hash_pair(branch, current)
        }
      compute_root(rest, index, level + 1, next)
    }
  }
}

/// Get the zero hash at a given level (for sparse tree nodes)
fn zero_hashes_at_level(level: Int) -> ByteArray {
  if level <= 0 {
    zero_hash
  } else {
    let child = zero_hashes_at_level(level - 1)
    hash_pair(child, child)
  }
}

/// Hash two nodes together
pub fn hash_pair(left: ByteArray, right: ByteArray) -> ByteArray {
  keccak_256(bytearray.concat(left, right))
}

/// Hash a leaf (domain separator for leaves)
pub fn hash_leaf(data: ByteArray) -> ByteArray {
  // Prefix with 0x00 to distinguish leaves from internal nodes
  keccak_256(bytearray.concat(#"00", data))
}

/// Power of 2
fn pow2(n: Int) -> Int {
  if n <= 0 {
    1
  } else {
    2 * pow2(n - 1)
  }
}

/// Verify a merkle proof
/// Returns true if the proof shows that `leaf` is at `index` in a tree with `root`
pub fn verify_proof(
  root_hash: ByteArray,
  leaf: ByteArray,
  index: Int,
  proof: List<ByteArray>,
) -> Bool {
  let computed = compute_root_from_proof(leaf, index, proof, 0)
  computed == root_hash
}

/// Compute root from a merkle proof
fn compute_root_from_proof(
  node: ByteArray,
  index: Int,
  proof: List<ByteArray>,
  level: Int,
) -> ByteArray {
  when proof is {
    [] -> node
    [sibling, ..rest] -> {
      let bit = index / pow2(level) % 2
      let parent =
        if bit == 0 {
          hash_pair(node, sibling)
        } else {
          hash_pair(sibling, node)
        }
      compute_root_from_proof(parent, index, rest, level + 1)
    }
  }
}
