//! `DefaultToken` native precompile — the base B-20 token variant.

mod dispatch;

mod evm;
pub use evm::DefaultTokenEvm;

mod storage;
pub use storage::{DEFAULT_TOKEN_ADDRESS, DefaultTokenStorage};

mod token;
pub use token::DefaultToken;
