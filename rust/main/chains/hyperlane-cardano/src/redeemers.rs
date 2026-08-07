/// Plutus CBOR constructor tag encoding.
///
/// Aiken sum type constructors map to CBOR alternative tags:
/// - Constructors 0–6 → tags 121–127
/// - Constructors 7+  → tags 1280 + (index - 7)
pub fn plutus_constr_tag(index: u64) -> u64 {
    if index <= 6 {
        121 + index
    } else {
        1280 + (index - 7)
    }
}

#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub enum MailboxRedeemerTag {
    Dispatch = 0,
    Process = 1,
    SetDefaultIsm = 2,
    TransferOwnership = 3,
}

impl MailboxRedeemerTag {
    pub fn plutus_tag(self) -> u64 {
        plutus_constr_tag(self as u64)
    }
}

#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub enum MultisigIsmRedeemerTag {
    Verify = 0,
    SetValidators = 1,
    SetThreshold = 2,
}

impl MultisigIsmRedeemerTag {
    pub fn plutus_tag(self) -> u64 {
        plutus_constr_tag(self as u64)
    }
}

#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub enum WarpRouteRedeemerTag {
    TransferRemote = 0,
    ReceiveTransfer = 1,
    EnrollRemoteRoute = 2,
}

impl WarpRouteRedeemerTag {
    pub fn plutus_tag(self) -> u64 {
        plutus_constr_tag(self as u64)
    }
}
