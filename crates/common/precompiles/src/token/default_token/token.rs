//! `DefaultToken` struct — the concrete B-20 token type.

use alloy_primitives::Address;

use crate::token::common::{
    Burnable, Configurable, Mintable, Pausable, Permittable, Redeemable, Token, TokenAccounting,
    Transferable,
};

/// EVM precompile for the Default B-20 token variant.
///
/// The generic `S` lets callers swap in an in-memory [`TokenAccounting`]
/// implementation for unit tests without touching real EVM storage. In
/// production, the storage adapter is bound to the address selected by the
/// dynamic precompile lookup.
#[derive(Debug, Clone)]
pub struct DefaultToken<S: TokenAccounting> {
    pub(super) accounting: S,
}

impl<S: TokenAccounting> DefaultToken<S> {
    /// Creates a `DefaultToken` backed by the provided storage adapter.
    ///
    /// Use this in tests to inject an in-memory [`TokenAccounting`] implementation.
    pub const fn with_storage(accounting: S) -> Self {
        Self { accounting }
    }
}

// ---------------------------------------------------------------------------
// Token: wire the accounting field and dynamic token address
// ---------------------------------------------------------------------------

impl<S: TokenAccounting> Token for DefaultToken<S> {
    type Accounting = S;

    fn accounting(&self) -> &S {
        &self.accounting
    }

    fn accounting_mut(&mut self) -> &mut S {
        &mut self.accounting
    }

    fn token_address(&self) -> Address {
        self.accounting.token_address()
    }
}

// ---------------------------------------------------------------------------
// Capability selection — DefaultToken opts in to all capabilities
// ---------------------------------------------------------------------------

impl<S: TokenAccounting> Transferable for DefaultToken<S> {}
impl<S: TokenAccounting> Mintable for DefaultToken<S> {}
impl<S: TokenAccounting> Burnable for DefaultToken<S> {}
impl<S: TokenAccounting> Redeemable for DefaultToken<S> {}
impl<S: TokenAccounting> Pausable for DefaultToken<S> {}
impl<S: TokenAccounting> Configurable for DefaultToken<S> {}
impl<S: TokenAccounting> Permittable for DefaultToken<S> {}
