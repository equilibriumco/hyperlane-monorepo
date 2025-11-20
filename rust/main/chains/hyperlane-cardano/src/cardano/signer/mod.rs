#[derive(Debug, Clone)]
pub struct Keypair(String);

impl Keypair {
    /// Recovers a `Keypair` from a byte array
    pub fn from_string(string: &str) -> Option<Self> {
        return Some(Keypair(string.to_string()));
    }
}
