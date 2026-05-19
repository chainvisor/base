//! `TokenFactory` native precompile — creates B-20 tokens at deterministic prefix-encoded addresses.

mod dispatch;

mod evm;
pub use evm::TokenFactoryEvm;

mod storage;
pub use storage::{FACTORY_ADDRESS, TokenFactory};

mod variant;
pub use variant::{
    DEFAULT_PREFIX, RESERVED_SIZE, SECURITY_PREFIX, STABLECOIN_PREFIX, TokenVariant,
    VARIANT_DEFAULT, VARIANT_NONE, VARIANT_SECURITY, VARIANT_STABLECOIN,
};
