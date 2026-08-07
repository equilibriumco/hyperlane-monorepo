use sha3::{Digest, Keccak256};
use std::collections::HashMap;
use thiserror::Error;

const DEPTH: usize = 128;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SmtError {
    #[error("Cannot prove non-membership for existing key")]
    KeyAlreadyPresent,
    #[error("SMT non-membership proof failed: computed root doesn't match expected root")]
    InvalidProof,
}

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

/// Compare keys in tree traversal order: bit 127 most significant, bit 0 least significant.
/// This ordering ensures each level's left/right split is a contiguous partition of the sorted
/// slice, enabling O(N log N) root and proof computation via binary search (partition_point).
fn cmp_tree_order(a: &[u8; 16], b: &[u8; 16]) -> std::cmp::Ordering {
    for bit in (0..DEPTH).rev() {
        match get_bit(a, bit).cmp(&get_bit(b, bit)) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }
    }
    std::cmp::Ordering::Equal
}

/// Compute the hash of a subtree at the given height from a tree-order sorted key slice.
/// O(N) per call; O(N * DEPTH) total across all recursive calls for a single root computation.
fn compute_subtree_hash_sorted(
    sorted: &[[u8; 16]],
    level: usize,
    zeros: &[[u8; 32]; DEPTH + 1],
) -> [u8; 32] {
    if sorted.is_empty() {
        return zeros[level];
    }
    if level == 0 {
        return PRESENT_LEAF;
    }
    let split_pos = sorted.partition_point(|k| !get_bit(k, level - 1));
    let left = compute_subtree_hash_sorted(&sorted[..split_pos], level - 1, zeros);
    let right = compute_subtree_hash_sorted(&sorted[split_pos..], level - 1, zeros);
    hash_pair(&left, &right)
}

/// Collect sibling hashes for a non-membership proof, top-down (to be reversed by caller).
/// At each level, computes the sibling subtree hash and recurses into the side containing key.
fn collect_proof_path(
    sorted: &[[u8; 16]],
    key: &[u8; 16],
    level: usize,
    zeros: &[[u8; 32]; DEPTH + 1],
    siblings: &mut Vec<[u8; 32]>,
) {
    if level == 0 {
        return;
    }
    let split_pos = sorted.partition_point(|k| !get_bit(k, level - 1));
    let (our_keys, sibling_keys) = if !get_bit(key, level - 1) {
        (&sorted[..split_pos], &sorted[split_pos..])
    } else {
        (&sorted[split_pos..], &sorted[..split_pos])
    };
    siblings.push(compute_subtree_hash_sorted(sibling_keys, level - 1, zeros));
    collect_proof_path(our_keys, key, level - 1, zeros, siblings);
}

/// Verify a non-membership proof against root and return the new root after inserting key.
fn verify_and_insert_key(
    root: &[u8; 32],
    key: &[u8; 16],
    proof: &[[u8; 32]],
) -> Result<[u8; 32], SmtError> {
    let mut old_hash = EMPTY_LEAF;
    let mut new_hash = PRESENT_LEAF;
    for (idx, sibling) in proof.iter().enumerate() {
        if !get_bit(key, idx) {
            old_hash = hash_pair(&old_hash, sibling);
            new_hash = hash_pair(&new_hash, sibling);
        } else {
            old_hash = hash_pair(sibling, &old_hash);
            new_hash = hash_pair(sibling, &new_hash);
        }
    }
    if &old_hash != root {
        return Err(SmtError::InvalidProof);
    }
    Ok(new_hash)
}

/// 128-bit key Sparse Merkle Tree for replay protection.
/// Keys are the first 16 bytes of message IDs; values are boolean (present/absent).
#[derive(Clone)]
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
        let sorted = self.sorted_keys_tree_order();
        compute_subtree_hash_sorted(&sorted, DEPTH, &self.zero_hashes)
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
    ) -> Result<[u8; 32], SmtError> {
        verify_and_insert_key(root, key, proof)
    }

    pub fn contains(&self, key: &[u8; 16]) -> bool {
        self.leaves.contains_key(key)
    }

    /// Generate a non-membership proof (128 sibling hashes, leaf→root order).
    pub fn prove_non_membership(&self, key: &[u8; 16]) -> Result<Vec<[u8; 32]>, SmtError> {
        if self.contains(key) {
            return Err(SmtError::KeyAlreadyPresent);
        }
        let sorted = self.sorted_keys_tree_order();
        let mut siblings = Vec::with_capacity(DEPTH);
        collect_proof_path(&sorted, key, DEPTH, &self.zero_hashes, &mut siblings);
        siblings.reverse();
        Ok(siblings)
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

    fn sorted_keys_tree_order(&self) -> Vec<[u8; 16]> {
        let mut keys: Vec<[u8; 16]> = self.leaves.keys().copied().collect();
        keys.sort_by(cmp_tree_order);
        keys
    }
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
        let proof = tree
            .prove_non_membership(&key)
            .expect("proof should be generated");
        assert_eq!(proof.len(), DEPTH);

        let new_root = SparseMerkleTree::verify_and_insert_static(&EMPTY_ROOT, &key, &proof)
            .expect("proof should verify");
        assert_ne!(new_root, EMPTY_ROOT);
    }

    #[test]
    fn test_proof_and_insert_consistency() {
        let mut tree = SparseMerkleTree::new();
        let key1 = [0x01u8; 16];
        let key2 = [0x02u8; 16];

        let proof1 = tree
            .prove_non_membership(&key1)
            .expect("proof should be generated");
        let root_after_1 = SparseMerkleTree::verify_and_insert_static(&tree.root(), &key1, &proof1)
            .expect("proof should verify");
        tree.insert(key1);
        assert_eq!(tree.root(), root_after_1);

        let proof2 = tree
            .prove_non_membership(&key2)
            .expect("proof should be generated");
        let root_after_2 = SparseMerkleTree::verify_and_insert_static(&tree.root(), &key2, &proof2)
            .expect("proof should verify");
        tree.insert(key2);
        assert_eq!(tree.root(), root_after_2);
    }

    #[test]
    fn test_cannot_prove_existing_key() {
        let mut tree = SparseMerkleTree::new();
        let key = [0xAB; 16];
        tree.insert(key);
        let err = tree
            .prove_non_membership(&key)
            .expect_err("existing key must fail");
        assert_eq!(err, SmtError::KeyAlreadyPresent);
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

    #[test]
    fn test_many_keys_proof_correctness() {
        let mut tree = SparseMerkleTree::new();
        for i in 0u32..50 {
            let mut key = [0u8; 16];
            key[0..4].copy_from_slice(&i.to_be_bytes());
            tree.insert(key);
        }

        let absent_key = [0xFFu8; 16];
        let proof = tree
            .prove_non_membership(&absent_key)
            .expect("proof should be generated");
        assert_eq!(proof.len(), DEPTH);

        let new_root =
            SparseMerkleTree::verify_and_insert_static(&tree.root(), &absent_key, &proof)
                .expect("proof should verify");
        tree.insert(absent_key);
        assert_eq!(tree.root(), new_root);
    }
}
