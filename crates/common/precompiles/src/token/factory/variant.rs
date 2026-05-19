//! B-20 token variant address derivation.

use alloy_primitives::{Address, B256, keccak256};
use alloy_sol_types::SolValue;

/// Address prefix for Default-variant tokens.
pub const DEFAULT_PREFIX: [u8; 12] = TokenVariant::Default.prefix();
/// Address prefix for Stablecoin-variant tokens.
pub const STABLECOIN_PREFIX: [u8; 12] = TokenVariant::Stablecoin.prefix();
/// Address prefix for Security-variant tokens.
pub const SECURITY_PREFIX: [u8; 12] = TokenVariant::Security.prefix();

/// Addresses whose lower-8-byte value is reserved for protocol bootstrap tokens.
pub const RESERVED_SIZE: u64 = 1024;

/// B-20 token variant encoded in the token address prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TokenVariant {
    /// Default-variant B-20 token.
    Default = 1,
    /// Stablecoin-variant B-20 token.
    Stablecoin = 2,
    /// Security-variant B-20 token.
    Security = 3,
}

impl TokenVariant {
    /// Returns this variant's 12-byte deterministic address prefix.
    pub const fn prefix(self) -> [u8; 12] {
        match self {
            Self::Default => [0xb0, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            Self::Stablecoin => [0xb0, 0x21, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            Self::Security => [0xb0, 0x22, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        }
    }

    /// Returns this variant's ABI discriminant.
    pub const fn discriminant(self) -> u8 {
        self as u8
    }

    /// Computes this variant's deterministic token address for `creator` and `salt`.
    ///
    /// Returns the address and the lower 8 bytes of the hash as a `u64` for the reserved-range
    /// check.
    pub fn compute_address(self, creator: Address, salt: B256) -> (Address, u64) {
        let hash = keccak256((creator, salt).abi_encode());

        let mut lower_bytes_buf = [0u8; 8];
        lower_bytes_buf.copy_from_slice(&hash[..8]);
        let lower_bytes = u64::from_be_bytes(lower_bytes_buf);

        let prefix = self.prefix();
        let mut addr_bytes = [0u8; 20];
        addr_bytes[..12].copy_from_slice(&prefix);
        addr_bytes[12..].copy_from_slice(&hash[..8]);

        (Address::from(addr_bytes), lower_bytes)
    }

    /// Returns the token variant encoded in `address`, if it has a valid B-20 prefix.
    pub fn from_address(address: Address) -> Option<Self> {
        let bytes = address.as_slice();
        if bytes[0] != 0xb0 || bytes[2..12] != [0u8; 10] {
            return None;
        }

        match bytes[1] {
            0x20 => Some(Self::Default),
            0x21 => Some(Self::Stablecoin),
            0x22 => Some(Self::Security),
            _ => None,
        }
    }

    /// Returns `true` when `address` has any B-20 token variant prefix.
    pub fn is_b20_address(address: Address) -> bool {
        Self::from_address(address).is_some()
    }
}
