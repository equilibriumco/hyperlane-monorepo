use sha3::{Digest, Keccak256};
use std::collections::HashMap;

const DEPTH: usize = 128;

fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    hasher.finalize().into()
}

pub const EMPTY_LEAF: [u8; 32] = [
    0xbc, 0x36, 0x78, 0x9e, 0x7a, 0x1e, 0x28, 0x14, 0x36, 0x46, 0x42, 0x29, 0x82, 0x8f, 0x81, 0x7d,
    0x66, 0x12, 0xf7, 0xb4, 0x77, 0xd6, 0x65, 0x91, 0xff, 0x96, 0xa9, 0xe0, 0x64, 0xbc, 0xc9, 0x8a,
];

const PRESENT_LEAF: [u8; 32] = [
    0x5f, 0xe7, 0xf9, 0x77, 0xe7, 0x1d, 0xba, 0x2e, 0xa1, 0xa6, 0x8e, 0x21, 0x05, 0x7b, 0xee, 0xbb,
    0x9b, 0xe2, 0xac, 0x30, 0xc6, 0x41, 0x0a, 0xa3, 0x8d, 0x4f, 0x3f, 0xbe, 0x41, 0xdc, 0xff, 0xd2,
];

pub const EMPTY_ROOT: [u8; 32] = [
    0x5c, 0x3c, 0xc3, 0x58, 0xc0, 0x60, 0x87, 0x7c, 0xed, 0x35, 0x94, 0x70, 0x91, 0xc4, 0x4c, 0x90,
    0x05, 0x94, 0xec, 0xe1, 0xe0, 0xa4, 0xad, 0xe2, 0x31, 0x43, 0xef, 0x57, 0xc3, 0xf7, 0x60, 0x0f,
];

fn hash_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(left);
    data[32..].copy_from_slice(right);
    keccak256(&data)
}

fn get_bit(key: &[u8; 16], index: usize) -> bool {
    let byte_val = key[index / 8];
    let bit_pos = 7 - (index % 8);
    (byte_val >> bit_pos) & 1 == 1
}

fn compute_zero_hashes() -> [[u8; 32]; DEPTH + 1] {
    let mut zeros = [[0u8; 32]; DEPTH + 1];
    zeros[0] = EMPTY_LEAF;
    for i in 1..=DEPTH {
        zeros[i] = hash_pair(&zeros[i - 1], &zeros[i - 1]);
    }
    zeros
}

/// 128-bit key Sparse Merkle Tree for replay protection.
/// Keys are the first 16 bytes of message IDs; values are boolean (present/absent).
pub struct SparseMerkleTree {
    leaves: HashMap<[u8; 16], bool>,
    zero_hashes: [[u8; 32]; DEPTH + 1],
}

impl Default for SparseMerkleTree {
    fn default() -> Self {
        Self::new()
    }
}

impl SparseMerkleTree {
    pub fn new() -> Self {
        Self {
            leaves: HashMap::new(),
            zero_hashes: compute_zero_hashes(),
        }
    }

    pub fn root(&self) -> [u8; 32] {
        if self.leaves.is_empty() {
            return EMPTY_ROOT;
        }
        self.compute_root_recursive(0, &self.sorted_keys())
    }

    pub fn insert(&mut self, key: [u8; 16]) {
        self.leaves.insert(key, true);
    }

    /// Static method matching the on-chain verification logic:
    /// verify old_root via non-membership proof and return new_root after insert.
    pub fn verify_and_insert_static(
        root: &[u8; 32],
        key: &[u8; 16],
        proof: &[[u8; 32]],
    ) -> [u8; 32] {
        SparseMerkleTreeVerifier::verify_and_insert(root, key, proof)
    }

    pub fn contains(&self, key: &[u8; 16]) -> bool {
        self.leaves.contains_key(key)
    }

    /// Generate a non-membership proof (128 sibling hashes, leaf→root order).
    /// Panics if the key is already in the tree.
    pub fn prove_non_membership(&self, key: &[u8; 16]) -> Vec<[u8; 32]> {
        assert!(
            !self.contains(key),
            "Cannot prove non-membership for existing key"
        );
        let mut proof = Vec::with_capacity(DEPTH);
        for level in 0..DEPTH {
            let sibling = self.compute_sibling(key, level);
            proof.push(sibling);
        }
        proof
    }

    /// Reconstruct SMT from a set of message IDs (first 16 bytes of each).
    pub fn from_message_ids(ids: &[[u8; 32]]) -> Self {
        let mut tree = Self::new();
        for id in ids {
            let mut key = [0u8; 16];
            key.copy_from_slice(&id[..16]);
            tree.insert(key);
        }
        tree
    }

    fn sorted_keys(&self) -> Vec<[u8; 16]> {
        let mut keys: Vec<[u8; 16]> = self.leaves.keys().copied().collect();
        keys.sort();
        keys
    }

    fn leaf_hash(&self, key: &[u8; 16]) -> [u8; 32] {
        if self.leaves.contains_key(key) {
            PRESENT_LEAF
        } else {
            EMPTY_LEAF
        }
    }

    fn compute_sibling(&self, key: &[u8; 16], level: usize) -> [u8; 32] {
        let mut sibling_key = *key;
        let byte_idx = level / 8;
        let bit_pos = 7 - (level % 8);
        sibling_key[byte_idx] ^= 1 << bit_pos;

        self.compute_subtree_hash(&sibling_key, level)
    }

    fn compute_subtree_hash(&self, prefix_key: &[u8; 16], level: usize) -> [u8; 32] {
        if level == 0 {
            return self.leaf_hash(prefix_key);
        }

        let keys_in_subtree: Vec<[u8; 16]> = self
            .leaves
            .keys()
            .filter(|k| self.shares_prefix(k, prefix_key, level))
            .copied()
            .collect();

        if keys_in_subtree.is_empty() {
            return self.zero_hashes[level];
        }

        let mut zero_key = *prefix_key;
        let byte_idx = (level - 1) / 8;
        let bit_pos = 7 - ((level - 1) % 8);

        let mut left_key = zero_key;
        left_key[byte_idx] &= !(1 << bit_pos);
        let mut right_key = zero_key;
        right_key[byte_idx] |= 1 << bit_pos;

        // Clear bits below level-1 for the prefix comparison
        for l in 0..(level - 1) {
            let bi = l / 8;
            let bp = 7 - (l % 8);
            left_key[bi] &= !(1 << bp);
            right_key[bi] &= !(1 << bp);
            zero_key[bi] &= !(1 << bp);
        }

        let left_hash = self.compute_subtree_hash(&left_key, level - 1);
        let right_hash = self.compute_subtree_hash(&right_key, level - 1);
        hash_pair(&left_hash, &right_hash)
    }

    fn shares_prefix(&self, a: &[u8; 16], b: &[u8; 16], level: usize) -> bool {
        for l in (level..DEPTH).rev() {
            if get_bit(a, l) != get_bit(b, l) {
                return false;
            }
        }
        true
    }

    fn compute_root_recursive(&self, _start_level: usize, _keys: &[[u8; 16]]) -> [u8; 32] {
        // Compute root by building proofs bottom-up.
        // For correctness, we verify by inserting all keys into an empty tree
        // and computing the root via the proof mechanism.
        let mut root = EMPTY_ROOT;
        let mut temp_tree = SparseMerkleTreeVerifier::new();
        for key in self.leaves.keys() {
            let proof = temp_tree.prove_non_membership_direct(key, &self.zero_hashes);
            root = SparseMerkleTreeVerifier::verify_and_insert(&root, key, &proof);
            temp_tree.insert(*key);
        }
        root
    }
}

/// Lightweight verifier used internally for root computation.
struct SparseMerkleTreeVerifier {
    leaves: HashMap<[u8; 16], bool>,
}

impl SparseMerkleTreeVerifier {
    fn new() -> Self {
        Self {
            leaves: HashMap::new(),
        }
    }

    fn insert(&mut self, key: [u8; 16]) {
        self.leaves.insert(key, true);
    }

    /// Compute a non-membership proof for a key against the current tree state.
    fn prove_non_membership_direct(
        &self,
        key: &[u8; 16],
        zero_hashes: &[[u8; 32]; DEPTH + 1],
    ) -> Vec<[u8; 32]> {
        let mut proof = Vec::with_capacity(DEPTH);
        for level in 0..DEPTH {
            let sibling = self.compute_sibling_direct(key, level, zero_hashes);
            proof.push(sibling);
        }
        proof
    }

    fn compute_sibling_direct(
        &self,
        key: &[u8; 16],
        level: usize,
        zero_hashes: &[[u8; 32]; DEPTH + 1],
    ) -> [u8; 32] {
        let mut sibling_prefix = *key;
        let byte_idx = level / 8;
        let bit_pos = 7 - (level % 8);
        sibling_prefix[byte_idx] ^= 1 << bit_pos;

        self.compute_subtree_direct(&sibling_prefix, level, zero_hashes)
    }

    fn compute_subtree_direct(
        &self,
        prefix_key: &[u8; 16],
        level: usize,
        zero_hashes: &[[u8; 32]; DEPTH + 1],
    ) -> [u8; 32] {
        if level == 0 {
            return if self.leaves.contains_key(prefix_key) {
                PRESENT_LEAF
            } else {
                EMPTY_LEAF
            };
        }

        let has_keys = self
            .leaves
            .keys()
            .any(|k| shares_prefix_static(k, prefix_key, level));

        if !has_keys {
            return zero_hashes[level];
        }

        let byte_idx = (level - 1) / 8;
        let bit_pos = 7 - ((level - 1) % 8);

        let mut left_key = *prefix_key;
        left_key[byte_idx] &= !(1 << bit_pos);
        let mut right_key = *prefix_key;
        right_key[byte_idx] |= 1 << bit_pos;

        for l in 0..(level - 1) {
            let bi = l / 8;
            let bp = 7 - (l % 8);
            left_key[bi] &= !(1 << bp);
            right_key[bi] &= !(1 << bp);
        }

        let left = self.compute_subtree_direct(&left_key, level - 1, zero_hashes);
        let right = self.compute_subtree_direct(&right_key, level - 1, zero_hashes);
        hash_pair(&left, &right)
    }

    fn verify_and_insert(root: &[u8; 32], key: &[u8; 16], proof: &[[u8; 32]]) -> [u8; 32] {
        let mut old_hash = EMPTY_LEAF;
        let mut new_hash = PRESENT_LEAF;

        for (idx, sibling) in proof.iter().enumerate() {
            let bit = get_bit(key, idx);
            if !bit {
                old_hash = hash_pair(&old_hash, sibling);
                new_hash = hash_pair(&new_hash, sibling);
            } else {
                old_hash = hash_pair(sibling, &old_hash);
                new_hash = hash_pair(sibling, &new_hash);
            }
        }

        assert_eq!(
            &old_hash, root,
            "SMT non-membership proof failed: computed root doesn't match"
        );
        new_hash
    }
}

fn shares_prefix_static(a: &[u8; 16], b: &[u8; 16], level: usize) -> bool {
    for l in (level..DEPTH).rev() {
        if get_bit(a, l) != get_bit(b, l) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_tree_root() {
        let tree = SparseMerkleTree::new();
        assert_eq!(tree.root(), EMPTY_ROOT);
    }

    #[test]
    fn test_constants_match_aiken() {
        assert_eq!(EMPTY_LEAF, keccak256(&[0x00]));
        assert_eq!(PRESENT_LEAF, keccak256(&[0x01]));

        let mut hash = EMPTY_LEAF;
        for _ in 0..DEPTH {
            hash = hash_pair(&hash, &hash);
        }
        assert_eq!(hash, EMPTY_ROOT);
    }

    #[test]
    fn test_single_insert_changes_root() {
        let mut tree = SparseMerkleTree::new();
        let key = [0u8; 16];
        tree.insert(key);
        assert_ne!(tree.root(), EMPTY_ROOT);
    }

    #[test]
    fn test_non_membership_proof_empty_tree() {
        let tree = SparseMerkleTree::new();
        let key = [0x42u8; 16];
        let proof = tree.prove_non_membership(&key);
        assert_eq!(proof.len(), DEPTH);

        // Verify the proof matches on-chain logic
        let new_root = SparseMerkleTreeVerifier::verify_and_insert(&EMPTY_ROOT, &key, &proof);
        assert_ne!(new_root, EMPTY_ROOT);
    }

    #[test]
    fn test_proof_and_insert_consistency() {
        let mut tree = SparseMerkleTree::new();
        let key1 = [0x01u8; 16];
        let key2 = [0x02u8; 16];

        // Insert key1 using proof
        let proof1 = tree.prove_non_membership(&key1);
        let root_after_1 =
            SparseMerkleTreeVerifier::verify_and_insert(&tree.root(), &key1, &proof1);
        tree.insert(key1);
        assert_eq!(tree.root(), root_after_1);

        // Insert key2 using proof
        let proof2 = tree.prove_non_membership(&key2);
        let root_after_2 =
            SparseMerkleTreeVerifier::verify_and_insert(&tree.root(), &key2, &proof2);
        tree.insert(key2);
        assert_eq!(tree.root(), root_after_2);
    }

    #[test]
    #[should_panic(expected = "Cannot prove non-membership")]
    fn test_cannot_prove_existing_key() {
        let mut tree = SparseMerkleTree::new();
        let key = [0xAB; 16];
        tree.insert(key);
        tree.prove_non_membership(&key);
    }

    #[test]
    fn test_from_message_ids() {
        let id1 = [0x11u8; 32];
        let id2 = [0x22u8; 32];
        let tree = SparseMerkleTree::from_message_ids(&[id1, id2]);
        assert!(tree.contains(&{
            let mut k = [0u8; 16];
            k.copy_from_slice(&id1[..16]);
            k
        }));
        assert!(tree.contains(&{
            let mut k = [0u8; 16];
            k.copy_from_slice(&id2[..16]);
            k
        }));
    }

    #[test]
    fn test_multiple_inserts_deterministic() {
        let keys: Vec<[u8; 16]> = (0..5u8)
            .map(|i| {
                let mut k = [0u8; 16];
                k[0] = i;
                k
            })
            .collect();

        let mut tree1 = SparseMerkleTree::new();
        for k in &keys {
            tree1.insert(*k);
        }

        let mut tree2 = SparseMerkleTree::new();
        for k in keys.iter().rev() {
            tree2.insert(*k);
        }

        assert_eq!(tree1.root(), tree2.root());
    }
}
