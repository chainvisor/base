//! `DefaultToken` native precompile — the base B-20 token variant.

mod dispatch;
mod evm;
mod storage;
mod token;

pub use evm::DefaultTokenEvm;
pub use storage::DefaultTokenStorage;
pub use token::DefaultToken;
