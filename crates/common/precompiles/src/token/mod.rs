//! Native precompiles for Base-native tokens (B-20).

mod abi;
pub use abi::{IDefaultToken, ITokenFactory};

mod common;
pub use common::{
    Burnable, CAPABILITY_CAP_MUTABLE, CAPABILITY_PAUSABLE, Configurable, Mintable, Pausable,
    Permittable, Redeemable, Token, TokenAccounting, Transferable,
};

mod default_token;
pub use default_token::{
    DEFAULT_TOKEN_ADDRESS, DefaultToken, DefaultTokenEvm, DefaultTokenStorage,
};

mod factory;
pub use factory::{
    DEFAULT_PREFIX, FACTORY_ADDRESS, RESERVED_SIZE, SECURITY_PREFIX, STABLECOIN_PREFIX,
    TokenFactory, TokenFactoryEvm, TokenVariant,
};
