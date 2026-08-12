//! Application-level message verification for Midnight.
//!
//! The relayer calls into [`ApplicationOperationVerifier`] before delivering a
//! message to its destination, so each chain integration can reject messages
//! that its on-chain runtime would refuse anyway (e.g. malformed bodies).
//!
//! For Midnight, the only structural constraint we enforce at this layer is
//! that warp-route messages must carry a parseable [`TokenMessage`]. Anything
//! else is left to the on-chain Mailbox + ISM + WarpRoute to handle.

use std::io::Cursor;

use async_trait::async_trait;
use derive_new::new;
use tracing::trace;

use hyperlane_core::{Decode, HyperlaneMessage};
use hyperlane_operation_verifier::{
    ApplicationOperationVerifier, ApplicationOperationVerifierReport,
};
use hyperlane_warp_route::TokenMessage;

const WARP_ROUTE_MARKER: &str = "/";

/// Application operation verifier for Midnight.
#[derive(new)]
pub struct MidnightApplicationOperationVerifier {}

#[async_trait]
impl ApplicationOperationVerifier for MidnightApplicationOperationVerifier {
    async fn verify(
        &self,
        app_context: &Option<String>,
        message: &HyperlaneMessage,
    ) -> Option<ApplicationOperationVerifierReport> {
        trace!(
            ?app_context,
            ?message,
            "Midnight application operation verifier",
        );

        Self::verify_message(app_context, message)
    }
}

impl MidnightApplicationOperationVerifier {
    fn verify_message(
        app_context: &Option<String>,
        message: &HyperlaneMessage,
    ) -> Option<ApplicationOperationVerifierReport> {
        use ApplicationOperationVerifierReport::MalformedMessage;

        let context = match app_context {
            Some(c) => c,
            None => return None,
        };

        if !context.contains(WARP_ROUTE_MARKER) {
            return None;
        }

        let mut reader = Cursor::new(message.body.as_slice());
        match TokenMessage::read_from(&mut reader) {
            Ok(_) => None,
            Err(_) => Some(MalformedMessage(message.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperlane_core::{HyperlaneMessage, H256};

    fn msg(body: Vec<u8>) -> HyperlaneMessage {
        HyperlaneMessage {
            version: 3,
            nonce: 0,
            origin: 1,
            sender: H256::zero(),
            destination: 2,
            recipient: H256::zero(),
            body,
        }
    }

    #[test]
    fn passes_without_app_context() {
        assert!(
            MidnightApplicationOperationVerifier::verify_message(&None, &msg(vec![0u8; 16]))
                .is_none()
        );
    }

    #[test]
    fn passes_non_warp_route_context() {
        assert!(MidnightApplicationOperationVerifier::verify_message(
            &Some("not-a-warp-route".to_string()),
            &msg(vec![0u8; 16]),
        )
        .is_none());
    }

    #[test]
    fn rejects_unparseable_warp_route_body() {
        let result = MidnightApplicationOperationVerifier::verify_message(
            &Some("warp/route".to_string()),
            &msg(vec![]),
        );
        assert!(matches!(
            result,
            Some(ApplicationOperationVerifierReport::MalformedMessage(_))
        ));
    }
}
