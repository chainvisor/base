use alloy_primitives::{Address, Bytes, U256, address};
use base_precompile_macros::contract;
use base_precompile_storage::{BasePrecompileError, Handler, Result};
use revm::state::Bytecode;

use super::variant::{RESERVED_SIZE, TokenVariant};
use crate::token::{DefaultTokenStorage, TokenAccounting, abi::ITokenFactory};

// ── Addresses ────────────────────────────────────────────────────────────────

/// Singleton precompile address for the `TokenFactory`.
pub const FACTORY_ADDRESS: Address = address!("b02f000000000000000000000000000000000000");

// ── Factory struct ────────────────────────────────────────────────────────────

/// The B-20 token factory precompile.
///
/// A stateless singleton — all token state lives at the individual token addresses.
/// This struct exists purely to group the factory logic and provide `emit_event` via the
/// `#[contract]` macro.
#[contract(addr = FACTORY_ADDRESS)]
pub struct TokenFactory {}

// ── Factory methods ───────────────────────────────────────────────────────────

impl<'a> TokenFactory<'a> {
    /// Creates a Default-variant token at a deterministic address derived from `(caller, salt)`.
    pub fn create_default(
        &mut self,
        caller: Address,
        call: ITokenFactory::createDefaultCall,
    ) -> Result<Address> {
        let p = call.params;

        // Input validation.
        if p.admin.is_zero() {
            return Err(BasePrecompileError::revert(ITokenFactory::ZeroAddress {}));
        }
        if p.supplyCap < p.initialSupply {
            return Err(BasePrecompileError::revert(ITokenFactory::InvalidSupplyCap {}));
        }

        let (token_address, lower_bytes) = TokenVariant::Default.compute_address(caller, p.salt);

        // Reserved-range guard.
        if lower_bytes < RESERVED_SIZE {
            return Err(BasePrecompileError::revert(ITokenFactory::AddressReserved {
                token: token_address,
            }));
        }

        // Collision guard: revert if code already exists at the target address.
        let already_deployed =
            self.storage.with_account_info(token_address, |info| Ok(!info.is_empty_code_hash()))?;
        if already_deployed {
            return Err(BasePrecompileError::revert(ITokenFactory::TokenAlreadyExists {
                token: token_address,
            }));
        }

        // Write the 0xEF stub — marks the address as occupied and signals the precompile fallback.
        let stub = Bytecode::new_legacy(Bytes::from_static(&[0xef]));
        self.storage.set_code(token_address, stub)?;

        // Initialize token storage at the token's own address.
        let mut token = DefaultTokenStorage::from_address(token_address, self.storage);
        token.name.write(p.name.clone())?;
        token.symbol.write(p.symbol.clone())?;
        token.decimals.write(p.decimals)?;
        token.supply_cap.write(p.supplyCap)?;
        token.capabilities.write(p.capabilities)?;
        token.minimum_redeemable.write(p.minimumRedeemable)?;
        token.contract_uri.write(p.contractURI.clone())?;

        if p.initialSupply > U256::ZERO {
            if p.initialSupplyRecipient.is_zero() {
                return Err(BasePrecompileError::revert(ITokenFactory::ZeroAddress {}));
            }
            token.total_supply.write(p.initialSupply)?;
            // TODO: Check if should emit a Transfer event
            token.set_balance(p.initialSupplyRecipient, p.initialSupply)?;
        }

        self.emit_event(ITokenFactory::DefaultTokenCreated {
            token: token_address,
            creator: caller,
            admin: p.admin,
            name: p.name,
            symbol: p.symbol,
            decimals: p.decimals,
            capabilities: p.capabilities,
            initialSupply: p.initialSupply,
            salt: p.salt,
        })?;

        Ok(token_address)
    }

    /// Returns whether `token` is a deployed B-20 token (prefix match + non-empty code).
    pub fn is_b20(&self, token: Address) -> Result<bool> {
        if !TokenVariant::is_b20_address(token) {
            return Ok(false);
        }
        self.storage.with_account_info(token, |info| Ok(!info.is_empty_code_hash()))
    }

    /// Returns the variant discriminant for `token` decoded from its address prefix.
    pub fn variant_of_token(&self, token: Address) -> Result<u8> {
        let Some(variant) = TokenVariant::from_address(token) else {
            return Ok(0);
        };
        Ok(variant.discriminant())
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use base_precompile_storage::{HashMapStorageProvider, StorageCtx};

    use super::*;

    fn make_params(
        name: &str,
        symbol: &str,
        salt: B256,
        initial_supply: U256,
        supply_cap: U256,
    ) -> ITokenFactory::createDefaultCall {
        ITokenFactory::createDefaultCall {
            params: ITokenFactory::CreateDefaultTokenParams {
                name: name.to_string(),
                symbol: symbol.to_string(),
                decimals: 18,
                admin: Address::repeat_byte(0xAB),
                capabilities: U256::ZERO,
                initialSupply: initial_supply,
                initialSupplyRecipient: Address::repeat_byte(0xCD),
                transferPolicyId: 1,
                supplyCap: supply_cap,
                minimumRedeemable: U256::ZERO,
                contractURI: "ipfs://test".to_string(),
                salt,
            },
        }
    }

    fn default_call(salt: B256) -> ITokenFactory::createDefaultCall {
        make_params("Test", "TST", salt, U256::from(1000), U256::MAX)
    }

    #[test]
    fn test_default_variant_address_is_deterministic() {
        let creator = Address::repeat_byte(0x11);
        let salt = B256::repeat_byte(0x22);
        let (a1, l1) = TokenVariant::Default.compute_address(creator, salt);
        let (a2, l2) = TokenVariant::Default.compute_address(creator, salt);
        assert_eq!(a1, a2);
        assert_eq!(l1, l2);
        assert!(TokenVariant::is_b20_address(a1));
        assert_eq!(TokenVariant::from_address(a1), Some(TokenVariant::Default));
    }

    #[test]
    fn test_different_salts_produce_different_addresses() {
        let creator = Address::repeat_byte(0x11);
        let (a1, _) = TokenVariant::Default.compute_address(creator, B256::repeat_byte(0x01));
        let (a2, _) = TokenVariant::Default.compute_address(creator, B256::repeat_byte(0x02));
        assert_ne!(a1, a2);
    }

    #[test]
    fn test_variants_produce_different_addresses_for_same_input() {
        let creator = Address::repeat_byte(0x11);
        let salt = B256::repeat_byte(0x33);
        let (def, _) = TokenVariant::Default.compute_address(creator, salt);
        let (sc, _) = TokenVariant::Stablecoin.compute_address(creator, salt);
        let (sec, _) = TokenVariant::Security.compute_address(creator, salt);
        assert_ne!(def, sc);
        assert_ne!(def, sec);
        assert_ne!(sc, sec);
        assert_eq!(TokenVariant::from_address(def), Some(TokenVariant::Default));
        assert_eq!(TokenVariant::from_address(sc), Some(TokenVariant::Stablecoin));
        assert_eq!(TokenVariant::from_address(sec), Some(TokenVariant::Security));
    }

    #[test]
    fn test_create_default_deploys_ef_stub() {
        let mut storage = HashMapStorageProvider::new(1);
        let caller = Address::repeat_byte(0x55);
        let salt = B256::repeat_byte(0xAA);
        let call = default_call(salt);
        let (expected_addr, _) = TokenVariant::Default.compute_address(caller, salt);

        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            factory.create_default(caller, call).unwrap();
            assert!(ctx.has_bytecode(expected_addr).unwrap());
        });
    }

    #[test]
    fn test_create_default_stores_metadata() {
        let mut storage = HashMapStorageProvider::new(1);
        let caller = Address::repeat_byte(0x55);
        let salt = B256::repeat_byte(0xBB);
        let call = make_params("My Token", "MYT", salt, U256::ZERO, U256::MAX);
        let (expected_addr, _) = TokenVariant::Default.compute_address(caller, salt);

        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            factory.create_default(caller, call).unwrap();

            let token = DefaultTokenStorage::from_address(expected_addr, ctx);
            assert_eq!(token.name.read().unwrap(), "My Token");
            assert_eq!(token.symbol.read().unwrap(), "MYT");
            assert_eq!(token.decimals.read().unwrap(), 18u8);
        });
    }

    #[test]
    fn test_create_default_mints_initial_supply() {
        let mut storage = HashMapStorageProvider::new(1);
        let caller = Address::repeat_byte(0x55);
        let salt = B256::repeat_byte(0xCC);
        let recipient = Address::repeat_byte(0xCD);
        let supply = U256::from(5_000u64);
        let call = make_params("Supply Token", "SUP", salt, supply, U256::MAX);
        let (expected_addr, _) = TokenVariant::Default.compute_address(caller, salt);

        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            factory.create_default(caller, call).unwrap();

            let token = DefaultTokenStorage::from_address(expected_addr, ctx);
            assert_eq!(token.total_supply.read().unwrap(), supply);
            assert_eq!(token.balance_of(recipient).unwrap(), supply);
        });
    }

    #[test]
    fn test_create_default_emits_event() {
        let mut storage = HashMapStorageProvider::new(1);
        let caller = Address::repeat_byte(0x55);
        let salt = B256::repeat_byte(0xDD);
        let call = default_call(salt);

        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            factory.create_default(caller, call).unwrap();
            let events = ctx.get_events(FACTORY_ADDRESS);
            assert_eq!(events.len(), 1);
        });
    }

    #[test]
    fn test_create_default_revert_if_salt_reused() {
        let mut storage = HashMapStorageProvider::new(1);
        let caller = Address::repeat_byte(0x55);
        let salt = B256::repeat_byte(0xEE);

        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            factory.create_default(caller, default_call(salt)).unwrap();
            let result = factory.create_default(caller, default_call(salt));
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_create_default_revert_zero_admin() {
        let mut storage = HashMapStorageProvider::new(1);
        let caller = Address::repeat_byte(0x55);
        let mut call = default_call(B256::repeat_byte(0x01));
        call.params.admin = Address::ZERO;

        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            let result = factory.create_default(caller, call);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_create_default_revert_supply_cap_below_initial() {
        let mut storage = HashMapStorageProvider::new(1);
        let caller = Address::repeat_byte(0x55);
        let call = make_params("T", "T", B256::repeat_byte(0x02), U256::from(100), U256::from(50));

        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            let result = factory.create_default(caller, call);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_is_b20_false_before_create() {
        let mut storage = HashMapStorageProvider::new(1);
        let creator = Address::repeat_byte(0x55);
        let (addr, _) = TokenVariant::Default.compute_address(creator, B256::repeat_byte(0xFF));

        StorageCtx::enter(&mut storage, |ctx| {
            let factory = TokenFactory::new(ctx);
            assert!(!factory.is_b20(addr).unwrap());
        });
    }

    #[test]
    fn test_is_b20_true_after_create() {
        let mut storage = HashMapStorageProvider::new(1);
        let caller = Address::repeat_byte(0x55);
        let salt = B256::repeat_byte(0x11);

        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            let token = factory.create_default(caller, default_call(salt)).unwrap();
            assert!(factory.is_b20(token).unwrap());
        });
    }

    #[test]
    fn test_variant_of_default_after_create() {
        let mut storage = HashMapStorageProvider::new(1);
        let caller = Address::repeat_byte(0x55);
        let salt = B256::repeat_byte(0x12);

        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            let token = factory.create_default(caller, default_call(salt)).unwrap();
            assert_eq!(
                factory.variant_of_token(token).unwrap(),
                TokenVariant::Default.discriminant()
            );
        });
    }

    #[test]
    fn test_is_b20_false_for_non_prefix_address() {
        let mut storage = HashMapStorageProvider::new(1);
        let random_addr = Address::repeat_byte(0x42);

        StorageCtx::enter(&mut storage, |ctx| {
            let factory = TokenFactory::new(ctx);
            assert!(!factory.is_b20(random_addr).unwrap());
        });
    }
}

// ── Integration tests ─────────────────────────────────────────────────────────
//
// These tests exercise the full token lifecycle: factory creation → metadata
// verification → mint → transfer → balance/supply accounting.
//
// The DefaultToken instance is constructed via `DefaultToken::with_storage(
// DefaultTokenStorage::from_address(token, ctx))`, which mirrors what the
// precompile-lookup fallback does when a call is routed to a B-20 address.

#[cfg(test)]
mod integration {
    use alloy_primitives::B256;
    use base_precompile_storage::{HashMapStorageProvider, StorageCtx};

    use super::*;
    use crate::token::{DefaultToken, DefaultTokenStorage, Mintable, Token, Transferable};

    /// Creates a token at the given address and returns a usable `DefaultToken` handle.
    fn token_at<'a>(addr: Address, ctx: StorageCtx<'a>) -> DefaultToken<DefaultTokenStorage<'a>> {
        DefaultToken::with_storage(DefaultTokenStorage::from_address(addr, ctx))
    }

    fn default_token_params(
        name: &str,
        symbol: &str,
        salt: B256,
    ) -> ITokenFactory::CreateDefaultTokenParams {
        ITokenFactory::CreateDefaultTokenParams {
            name: name.to_string(),
            symbol: symbol.to_string(),
            decimals: 18,
            admin: Address::repeat_byte(0xAD),
            capabilities: U256::ZERO,
            initialSupply: U256::ZERO,
            initialSupplyRecipient: Address::repeat_byte(0xCD),
            transferPolicyId: 1,
            supplyCap: U256::MAX,
            minimumRedeemable: U256::from(10u64),
            contractURI: "ipfs://QmTest".to_string(),
            salt,
        }
    }

    fn create_token(
        factory: &mut TokenFactory<'_>,
        params: ITokenFactory::CreateDefaultTokenParams,
    ) -> Address {
        let caller = Address::repeat_byte(0xCA);
        let call = ITokenFactory::createDefaultCall { params };
        factory.create_default(caller, call).unwrap()
    }

    // ── metadata ──────────────────────────────────────────────────────────────

    /// All metadata fields set at creation must be readable from the token address.
    #[test]
    fn test_metadata_all_fields() {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            let mut params = default_token_params("USD Coin", "USDC", B256::repeat_byte(0x01));
            params.decimals = 6;
            params.initialSupply = U256::from(1_000_000u64);
            params.capabilities = U256::from(0b11u64); // PAUSABLE | CAP_MUTABLE
            params.supplyCap = U256::from(u128::MAX);
            let token_addr = create_token(&mut factory, params);

            let t = DefaultTokenStorage::from_address(token_addr, ctx);

            assert_eq!(t.name.read().unwrap(), "USD Coin");
            assert_eq!(t.symbol.read().unwrap(), "USDC");
            assert_eq!(t.decimals.read().unwrap(), 6u8);
            assert_eq!(t.capabilities.read().unwrap(), U256::from(0b11u64));
            assert_eq!(t.supply_cap.read().unwrap(), U256::from(u128::MAX));
            assert_eq!(t.minimum_redeemable.read().unwrap(), U256::from(10u64));
            assert_eq!(t.contract_uri.read().unwrap(), "ipfs://QmTest");
            assert_eq!(t.total_supply.read().unwrap(), U256::from(1_000_000u64));
        });
    }

    // ── transfer ──────────────────────────────────────────────────────────────

    /// A successful transfer moves balance from sender to receiver and leaves
    /// total supply unchanged.
    #[test]
    fn test_transfer_moves_balance() {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            let mut params = default_token_params("Test Token", "TST", B256::repeat_byte(0x02));
            params.initialSupply = U256::from(1_000u64);
            let token_addr = create_token(&mut factory, params);

            let sender = Address::repeat_byte(0xCD); // initialSupplyRecipient
            let receiver = Address::repeat_byte(0xBB);
            let amount = U256::from(300u64);

            let mut token = token_at(token_addr, ctx);

            // Pre-transfer state.
            assert_eq!(token.accounting().balance_of(sender).unwrap(), U256::from(1_000u64));
            assert_eq!(token.accounting().balance_of(receiver).unwrap(), U256::ZERO);
            assert_eq!(token.accounting().total_supply().unwrap(), U256::from(1_000u64));

            token.transfer(sender, receiver, amount).unwrap();

            // Post-transfer state.
            assert_eq!(token.accounting().balance_of(sender).unwrap(), U256::from(700u64));
            assert_eq!(token.accounting().balance_of(receiver).unwrap(), U256::from(300u64));
            assert_eq!(token.accounting().total_supply().unwrap(), U256::from(1_000u64));
        });
    }

    /// Transferring more than the sender's balance reverts.
    #[test]
    fn test_transfer_insufficient_balance_reverts() {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            let mut params = default_token_params("T", "T", B256::repeat_byte(0x03));
            params.initialSupply = U256::from(100u64);
            let token_addr = create_token(&mut factory, params);

            let sender = Address::repeat_byte(0xCD);
            let receiver = Address::repeat_byte(0xBB);

            let mut token = token_at(token_addr, ctx);
            let result = token.transfer(sender, receiver, U256::from(101u64));
            assert!(result.is_err());

            // Balance must be unchanged.
            assert_eq!(token.accounting().balance_of(sender).unwrap(), U256::from(100u64));
        });
    }

    // ── mint ──────────────────────────────────────────────────────────────────

    /// Minting increases total supply and credits the recipient.
    #[test]
    fn test_mint_increases_supply() {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            let mut params = default_token_params("Mintable", "MNT", B256::repeat_byte(0x04));
            params.initialSupply = U256::from(500u64);
            let token_addr = create_token(&mut factory, params);

            let recipient = Address::repeat_byte(0xEE);
            let mut token = token_at(token_addr, ctx);

            token.mint(recipient, U256::from(200u64)).unwrap();

            assert_eq!(token.accounting().total_supply().unwrap(), U256::from(700u64));
            assert_eq!(token.accounting().balance_of(recipient).unwrap(), U256::from(200u64));
        });
    }

    /// Minting beyond the supply cap reverts.
    #[test]
    fn test_mint_beyond_supply_cap_reverts() {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            let cap = U256::from(1_000u64);
            let mut params = default_token_params("Capped", "CAP", B256::repeat_byte(0x05));
            params.initialSupply = U256::from(900u64);
            params.supplyCap = cap;
            let token_addr = create_token(&mut factory, params);

            let recipient = Address::repeat_byte(0xEE);
            let mut token = token_at(token_addr, ctx);

            // 101 would push supply to 1_001 > cap 1_000.
            let result = token.mint(recipient, U256::from(101u64));
            assert!(result.is_err());

            assert_eq!(token.accounting().total_supply().unwrap(), U256::from(900u64));
        });
    }

    // ── end-to-end ────────────────────────────────────────────────────────────

    /// Full lifecycle: create → verify metadata → transfer partial → mint more →
    /// verify final balances and supply.
    #[test]
    fn test_full_lifecycle() {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, |ctx| {
            let mut factory = TokenFactory::new(ctx);
            let alice = Address::repeat_byte(0xCD); // initialSupplyRecipient
            let bob = Address::repeat_byte(0xBB);
            let charlie = Address::repeat_byte(0xCC);

            let mut params = default_token_params("My Stablecoin", "MSC", B256::repeat_byte(0x06));
            params.decimals = 6;
            params.initialSupply = U256::from(10_000u64);
            params.capabilities = U256::from(0b11u64);
            params.supplyCap = U256::from(1_000_000u64);
            let token_addr = create_token(&mut factory, params);

            // ── verify factory state ──────────────────────────────────────────
            assert!(factory.is_b20(token_addr).unwrap(), "should be B-20");
            assert_eq!(
                factory.variant_of_token(token_addr).unwrap(),
                TokenVariant::Default.discriminant()
            );

            // ── verify 0xEF stub at token address ────────────────────────────
            assert!(ctx.has_bytecode(token_addr).unwrap(), "0xEF stub should be present");

            // ── verify metadata ───────────────────────────────────────────────
            let storage_handle = DefaultTokenStorage::from_address(token_addr, ctx);
            assert_eq!(storage_handle.name.read().unwrap(), "My Stablecoin");
            assert_eq!(storage_handle.symbol.read().unwrap(), "MSC");
            assert_eq!(storage_handle.decimals.read().unwrap(), 6u8);
            assert_eq!(storage_handle.supply_cap.read().unwrap(), U256::from(1_000_000u64));
            assert_eq!(storage_handle.capabilities.read().unwrap(), U256::from(0b11u64));

            // ── alice → bob: 4_000 tokens ─────────────────────────────────────
            let mut token = token_at(token_addr, ctx);
            token.transfer(alice, bob, U256::from(4_000u64)).unwrap();

            assert_eq!(token.accounting().balance_of(alice).unwrap(), U256::from(6_000u64));
            assert_eq!(token.accounting().balance_of(bob).unwrap(), U256::from(4_000u64));
            assert_eq!(token.accounting().total_supply().unwrap(), U256::from(10_000u64));

            // ── bob → charlie: 1_500 tokens ───────────────────────────────────
            token.transfer(bob, charlie, U256::from(1_500u64)).unwrap();

            assert_eq!(token.accounting().balance_of(bob).unwrap(), U256::from(2_500u64));
            assert_eq!(token.accounting().balance_of(charlie).unwrap(), U256::from(1_500u64));

            // ── mint 5_000 more to alice ───────────────────────────────────────
            token.mint(alice, U256::from(5_000u64)).unwrap();

            assert_eq!(token.accounting().balance_of(alice).unwrap(), U256::from(11_000u64));
            assert_eq!(token.accounting().total_supply().unwrap(), U256::from(15_000u64));

            // Final balances must sum to total supply.
            let alice_bal = token.accounting().balance_of(alice).unwrap();
            let bob_bal = token.accounting().balance_of(bob).unwrap();
            let charlie_bal = token.accounting().balance_of(charlie).unwrap();
            assert_eq!(alice_bal + bob_bal + charlie_bal, U256::from(15_000u64));
        });
    }
}
