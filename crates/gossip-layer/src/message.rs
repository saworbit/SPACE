//! Message signing and verification for gossip protocol.

use hmac::{Hmac, Mac};
use mesh_core::{CoreError, GossipMessage, Result};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// A signed gossip message with authentication.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedMessage {
    /// The actual message payload
    pub message: GossipMessage,

    /// HMAC signature for authentication
    pub signature: Vec<u8>,

    /// Message ID for deduplication
    pub message_id: String,

    /// Time-to-live (max hops remaining)
    pub ttl: u32,

    /// Sender peer ID
    pub sender: String,

    /// Unix timestamp
    pub timestamp: u64,
}

impl SignedMessage {
    /// Create a new signed message.
    ///
    /// # Arguments
    ///
    /// * `message` - The gossip message to sign
    /// * `sender` - Peer ID of the sender
    /// * `ttl` - Time-to-live for the message
    /// * `signing_key` - Secret key for HMAC signing
    pub fn new(
        message: GossipMessage,
        sender: String,
        ttl: u32,
        signing_key: &[u8],
    ) -> Result<Self> {
        let timestamp = crate::current_timestamp();
        let message_id = generate_message_id(&message, &sender, timestamp);

        // Serialize message for signing
        let serialized = bincode::serialize(&message)
            .map_err(|e| CoreError::SerializationError(e.to_string()))?;

        // Create HMAC signature
        let mut mac = HmacSha256::new_from_slice(signing_key)
            .map_err(|e| CoreError::AuthError(e.to_string()))?;
        mac.update(&serialized);
        mac.update(message_id.as_bytes());
        mac.update(&timestamp.to_le_bytes());

        let signature = mac.finalize().into_bytes().to_vec();

        Ok(Self {
            message,
            signature,
            message_id,
            ttl,
            sender,
            timestamp,
        })
    }

    /// Verify the message signature.
    ///
    /// # Arguments
    ///
    /// * `signing_key` - Secret key for HMAC verification
    pub fn verify(&self, signing_key: &[u8]) -> Result<()> {
        verify_message(self, signing_key)
    }

    /// Decrement TTL and return whether the message should be propagated.
    pub fn decrement_ttl(&mut self) -> bool {
        if self.ttl > 0 {
            self.ttl -= 1;
            true
        } else {
            false
        }
    }
}

/// Verify a signed message.
///
/// # Arguments
///
/// * `signed_msg` - The signed message to verify
/// * `signing_key` - Secret key for HMAC verification
pub fn verify_message(signed_msg: &SignedMessage, signing_key: &[u8]) -> Result<()> {
    // Serialize message
    let serialized = bincode::serialize(&signed_msg.message)
        .map_err(|e| CoreError::SerializationError(e.to_string()))?;

    // Create HMAC verifier
    let mut mac =
        HmacSha256::new_from_slice(signing_key).map_err(|e| CoreError::AuthError(e.to_string()))?;
    mac.update(&serialized);
    mac.update(signed_msg.message_id.as_bytes());
    mac.update(&signed_msg.timestamp.to_le_bytes());

    // Verify signature
    mac.verify_slice(&signed_msg.signature)
        .map_err(|_| CoreError::AuthError("Invalid message signature".to_string()))?;

    Ok(())
}

/// Generate a unique message ID.
fn generate_message_id(message: &GossipMessage, sender: &str, timestamp: u64) -> String {
    use sha2::Digest;

    let mut hasher = Sha256::new();

    // Hash message content
    if let Ok(serialized) = bincode::serialize(message) {
        hasher.update(&serialized);
    }

    // Add sender and timestamp
    hasher.update(sender.as_bytes());
    hasher.update(timestamp.to_le_bytes());

    // Return hex digest
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_core::GossipMessage;

    #[test]
    fn test_sign_and_verify() {
        let key = b"test_signing_key_32_bytes_long!!";
        let message = GossipMessage::Heartbeat {
            peer_id: "test".to_string(),
            storage_usage: 1024,
            timestamp: 12345,
        };

        let signed = SignedMessage::new(message.clone(), "sender-1".to_string(), 10, key).unwrap();

        assert!(signed.verify(key).is_ok());
    }

    #[test]
    fn test_invalid_signature() {
        let key = b"test_signing_key_32_bytes_long!!";
        let wrong_key = b"wrong_key_32_bytes_long_instead!";

        let message = GossipMessage::Heartbeat {
            peer_id: "test".to_string(),
            storage_usage: 1024,
            timestamp: 12345,
        };

        let signed = SignedMessage::new(message.clone(), "sender-1".to_string(), 10, key).unwrap();

        assert!(signed.verify(wrong_key).is_err());
    }

    #[test]
    fn test_ttl_decrement() {
        let key = b"test_signing_key_32_bytes_long!!";
        let message = GossipMessage::Heartbeat {
            peer_id: "test".to_string(),
            storage_usage: 1024,
            timestamp: 12345,
        };

        let mut signed =
            SignedMessage::new(message.clone(), "sender-1".to_string(), 2, key).unwrap();

        assert_eq!(signed.ttl, 2);
        assert!(signed.decrement_ttl());
        assert_eq!(signed.ttl, 1);
        assert!(signed.decrement_ttl());
        assert_eq!(signed.ttl, 0);
        assert!(!signed.decrement_ttl());
    }

    #[test]
    fn test_message_id_generation() {
        let message = GossipMessage::Heartbeat {
            peer_id: "test".to_string(),
            storage_usage: 1024,
            timestamp: 12345,
        };

        let id1 = generate_message_id(&message, "sender-1", 1000);
        let id2 = generate_message_id(&message, "sender-1", 1000);
        let id3 = generate_message_id(&message, "sender-2", 1000);

        // Same input should produce same ID
        assert_eq!(id1, id2);

        // Different sender should produce different ID
        assert_ne!(id1, id3);
    }
}
