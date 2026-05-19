//! Native ERC20 precompile RPC client helpers.

use std::time::Duration;

use alloy_consensus::SignableTransaction;
use alloy_eips::eip2718::Encodable2718;
use alloy_network::ReceiptResponse;
use alloy_primitives::{Address, B256, Bytes, U256, address};
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_types_eth::TransactionInput;
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, sol};
use base_common_network::Base;
use base_common_rpc_types::BaseTransactionRequest;
use eyre::{Result, WrapErr, ensure};
use tokio::time::{sleep, timeout};

sol! {
    interface INativeErc20 {
        function mint(address to, uint256 amount) external;
        function transfer(address to, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }

    interface INativeErc20Factory {
        struct CreateDefaultTokenParams {
            string name;
            string symbol;
            uint8 decimals;
            address admin;
            uint256 capabilities;
            uint256 initialSupply;
            address initialSupplyRecipient;
            uint64 transferPolicyId;
            uint256 supplyCap;
            uint256 minimumRedeemable;
            string contractURI;
            bytes32 salt;
        }

        function createDefault(CreateDefaultTokenParams calldata params) external returns (address token);
        function predictDefaultAddress(address creator, bytes32 salt) external view returns (address);
    }
}

pub use INativeErc20Factory::CreateDefaultTokenParams as NativeErc20CreateDefaultTokenParams;

/// RPC client for the native ERC20 factory precompile.
#[derive(Debug)]
pub struct NativeErc20Factory<'a> {
    provider: &'a RootProvider<Base>,
    signer: &'a PrivateKeySigner,
    chain_id: u64,
    gas_limit: u64,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
    receipt_timeout: Duration,
}

impl<'a> NativeErc20Factory<'a> {
    /// Native ERC20 factory precompile address.
    pub const ADDRESS: Address = address!("0xb02f000000000000000000000000000000000000");

    /// Default gas limit used when sending native ERC20 factory transactions.
    pub const DEFAULT_GAS_LIMIT: u64 = 10_000_000;

    /// Default max fee per gas used when sending native ERC20 factory transactions.
    pub const DEFAULT_MAX_FEE_PER_GAS: u128 = 1_000_000_000;

    /// Default priority fee per gas used when sending native ERC20 factory transactions.
    pub const DEFAULT_MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000;

    /// Default receipt timeout used after sending native ERC20 factory transactions.
    pub const DEFAULT_RECEIPT_TIMEOUT: Duration = Duration::from_secs(60);

    /// Creates a native ERC20 factory client.
    pub const fn new(
        provider: &'a RootProvider<Base>,
        signer: &'a PrivateKeySigner,
        chain_id: u64,
    ) -> Self {
        Self {
            provider,
            signer,
            chain_id,
            gas_limit: Self::DEFAULT_GAS_LIMIT,
            max_fee_per_gas: Self::DEFAULT_MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: Self::DEFAULT_MAX_PRIORITY_FEE_PER_GAS,
            receipt_timeout: Self::DEFAULT_RECEIPT_TIMEOUT,
        }
    }

    /// Sets the receipt timeout used after sending native ERC20 factory transactions.
    pub const fn with_receipt_timeout(mut self, receipt_timeout: Duration) -> Self {
        self.receipt_timeout = receipt_timeout;
        self
    }

    /// Creates a default native ERC20 token and returns its dynamic token address.
    pub async fn create_default(
        &self,
        params: NativeErc20CreateDefaultTokenParams,
    ) -> Result<Address> {
        let token = self.predict_default_address(self.signer.address(), params.salt).await?;
        self.send_call(
            INativeErc20Factory::createDefaultCall { params },
            "create native ERC20 token",
        )
        .await?;
        Ok(token)
    }

    /// Returns the address a default token creation would produce.
    pub async fn predict_default_address(&self, creator: Address, salt: B256) -> Result<Address> {
        let output =
            self.call(INativeErc20Factory::predictDefaultAddressCall { creator, salt }).await?;
        INativeErc20Factory::predictDefaultAddressCall::abi_decode_returns(output.as_ref())
            .wrap_err("Failed to decode predictDefaultAddress")
    }

    /// Executes an `eth_call` against the native ERC20 factory precompile.
    pub async fn call<C>(&self, call: C) -> Result<Bytes>
    where
        C: SolCall,
    {
        let request = BaseTransactionRequest::default()
            .from(self.signer.address())
            .to(Self::ADDRESS)
            .input(TransactionInput::new(Bytes::from(call.abi_encode())));

        self.provider.call(request).await.wrap_err("native ERC20 factory eth_call failed")
    }

    /// Signs, sends, and waits for a native ERC20 factory transaction.
    pub async fn send_call<C>(&self, call: C, label: &'static str) -> Result<()>
    where
        C: SolCall,
    {
        let nonce = self.provider.get_transaction_count(self.signer.address()).await?;
        let (raw_tx, expected_tx_hash) =
            self.create_signed_tx(nonce, Bytes::from(call.abi_encode())).wrap_err(label)?;

        let pending_tx = self
            .provider
            .send_raw_transaction(&raw_tx)
            .await
            .wrap_err_with(|| format!("Failed to send {label} transaction"))?;
        let tx_hash = *pending_tx.tx_hash();
        ensure!(tx_hash == expected_tx_hash, "{label} transaction hash mismatch");

        let receipt = timeout(self.receipt_timeout, async {
            loop {
                if let Some(receipt) = self.provider.get_transaction_receipt(tx_hash).await? {
                    return Ok::<_, eyre::Error>(receipt);
                }
                sleep(Duration::from_secs(1)).await;
            }
        })
        .await
        .wrap_err_with(|| format!("{label} receipt timed out"))?
        .wrap_err_with(|| format!("Failed to get {label} receipt"))?;

        ensure!(receipt.status(), "{label} transaction reverted");
        ensure!(receipt.inner.to == Some(Self::ADDRESS), "{label} receipt target mismatch");

        Ok(())
    }

    /// Creates a signed transaction targeting the native ERC20 factory precompile.
    pub fn create_signed_tx(&self, nonce: u64, input: Bytes) -> Result<(Bytes, B256)> {
        let tx_request = BaseTransactionRequest::default()
            .from(self.signer.address())
            .to(Self::ADDRESS)
            .value(U256::ZERO)
            .transaction_type(2)
            .gas_limit(self.gas_limit)
            .max_fee_per_gas(self.max_fee_per_gas)
            .max_priority_fee_per_gas(self.max_priority_fee_per_gas)
            .chain_id(self.chain_id)
            .nonce(nonce)
            .input(TransactionInput::new(input));

        let tx = tx_request.build_typed_tx().map_err(|tx| {
            eyre::eyre!("invalid native ERC20 factory transaction request: {tx:?}")
        })?;
        let signature = self.signer.sign_hash_sync(&tx.signature_hash())?;
        let signed_tx = tx.into_signed(signature);
        let tx_hash = *signed_tx.hash();
        let raw_tx = signed_tx.encoded_2718().into();

        Ok((raw_tx, tx_hash))
    }
}

/// RPC client for the native ERC20 precompile.
#[derive(Debug)]
pub struct NativeErc20Precompile<'a> {
    provider: &'a RootProvider<Base>,
    signer: &'a PrivateKeySigner,
    chain_id: u64,
    address: Address,
    gas_limit: u64,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
    receipt_timeout: Duration,
}

impl<'a> NativeErc20Precompile<'a> {
    /// Default gas limit used when sending native ERC20 transactions.
    pub const DEFAULT_GAS_LIMIT: u64 = 10_000_000;

    /// Default max fee per gas used when sending native ERC20 transactions.
    pub const DEFAULT_MAX_FEE_PER_GAS: u128 = 1_000_000_000;

    /// Default priority fee per gas used when sending native ERC20 transactions.
    pub const DEFAULT_MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000;

    /// Default receipt timeout used after sending native ERC20 transactions.
    pub const DEFAULT_RECEIPT_TIMEOUT: Duration = Duration::from_secs(60);

    /// Creates a native ERC20 precompile client.
    pub const fn new(
        provider: &'a RootProvider<Base>,
        signer: &'a PrivateKeySigner,
        chain_id: u64,
        address: Address,
    ) -> Self {
        Self {
            provider,
            signer,
            chain_id,
            address,
            gas_limit: Self::DEFAULT_GAS_LIMIT,
            max_fee_per_gas: Self::DEFAULT_MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: Self::DEFAULT_MAX_PRIORITY_FEE_PER_GAS,
            receipt_timeout: Self::DEFAULT_RECEIPT_TIMEOUT,
        }
    }

    /// Returns the dynamic native ERC20 token address this client targets.
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Sets the gas limit used for native ERC20 transactions.
    pub const fn with_gas_limit(mut self, gas_limit: u64) -> Self {
        self.gas_limit = gas_limit;
        self
    }

    /// Sets the receipt timeout used after sending native ERC20 transactions.
    pub const fn with_receipt_timeout(mut self, receipt_timeout: Duration) -> Self {
        self.receipt_timeout = receipt_timeout;
        self
    }

    /// Sets the max fee per gas used for native ERC20 transactions.
    pub const fn with_max_fee_per_gas(mut self, max_fee_per_gas: u128) -> Self {
        self.max_fee_per_gas = max_fee_per_gas;
        self
    }

    /// Sets the priority fee per gas used for native ERC20 transactions.
    pub const fn with_max_priority_fee_per_gas(mut self, max_priority_fee_per_gas: u128) -> Self {
        self.max_priority_fee_per_gas = max_priority_fee_per_gas;
        self
    }

    /// Waits for the precompile address to return non-empty bytecode.
    pub async fn wait_for_code(
        &self,
        wait_timeout: Duration,
        poll_interval: Duration,
    ) -> Result<()> {
        timeout(wait_timeout, async {
            loop {
                let code = self.provider.get_code_at(self.address).await?;
                if !code.is_empty() {
                    return Ok::<_, eyre::Error>(());
                }
                sleep(poll_interval).await;
            }
        })
        .await
        .wrap_err("Timed out waiting for native ERC20 precompile code")?
    }

    /// Reads the native ERC20 balance for an account.
    pub async fn balance_of(&self, account: Address) -> Result<U256> {
        let output = self.call(INativeErc20::balanceOfCall { account }).await?;
        INativeErc20::balanceOfCall::abi_decode_returns(output.as_ref())
            .wrap_err("Failed to decode balanceOf")
    }

    /// Mints native ERC20 tokens to an account.
    pub async fn mint(&self, to: Address, amount: U256) -> Result<()> {
        self.send_call(INativeErc20::mintCall { to, amount }, "mint native ERC20").await
    }

    /// Transfers native ERC20 tokens.
    pub async fn transfer(&self, to: Address, amount: U256) -> Result<()> {
        self.send_call(INativeErc20::transferCall { to, amount }, "transfer native ERC20").await
    }

    /// Executes an `eth_call` against the native ERC20 precompile.
    pub async fn call<C>(&self, call: C) -> Result<Bytes>
    where
        C: SolCall,
    {
        let request = BaseTransactionRequest::default()
            .from(self.signer.address())
            .to(self.address)
            .input(TransactionInput::new(Bytes::from(call.abi_encode())));

        self.provider.call(request).await.wrap_err("native ERC20 eth_call failed")
    }

    /// Signs, sends, and waits for a native ERC20 precompile transaction.
    pub async fn send_call<C>(&self, call: C, label: &'static str) -> Result<()>
    where
        C: SolCall,
    {
        let nonce = self.provider.get_transaction_count(self.signer.address()).await?;
        let (raw_tx, expected_tx_hash) =
            self.create_signed_tx(nonce, Bytes::from(call.abi_encode())).wrap_err(label)?;

        let pending_tx = self
            .provider
            .send_raw_transaction(&raw_tx)
            .await
            .wrap_err_with(|| format!("Failed to send {label} transaction"))?;
        let tx_hash = *pending_tx.tx_hash();
        ensure!(tx_hash == expected_tx_hash, "{label} transaction hash mismatch");

        let receipt = timeout(self.receipt_timeout, async {
            loop {
                if let Some(receipt) = self.provider.get_transaction_receipt(tx_hash).await? {
                    return Ok::<_, eyre::Error>(receipt);
                }
                sleep(Duration::from_secs(1)).await;
            }
        })
        .await
        .wrap_err_with(|| format!("{label} receipt timed out"))?
        .wrap_err_with(|| format!("Failed to get {label} receipt"))?;

        ensure!(receipt.status(), "{label} transaction reverted");
        ensure!(receipt.inner.to == Some(self.address), "{label} receipt target mismatch");

        Ok(())
    }

    /// Creates a signed transaction targeting the native ERC20 precompile.
    pub fn create_signed_tx(&self, nonce: u64, input: Bytes) -> Result<(Bytes, B256)> {
        let tx_request = BaseTransactionRequest::default()
            .from(self.signer.address())
            .to(self.address)
            .value(U256::ZERO)
            .transaction_type(2)
            .gas_limit(self.gas_limit)
            .max_fee_per_gas(self.max_fee_per_gas)
            .max_priority_fee_per_gas(self.max_priority_fee_per_gas)
            .chain_id(self.chain_id)
            .nonce(nonce)
            .input(TransactionInput::new(input));

        let tx = tx_request
            .build_typed_tx()
            .map_err(|tx| eyre::eyre!("invalid native ERC20 transaction request: {tx:?}"))?;
        let signature = self.signer.sign_hash_sync(&tx.signature_hash())?;
        let signed_tx = tx.into_signed(signature);
        let tx_hash = *signed_tx.hash();
        let raw_tx = signed_tx.encoded_2718().into();

        Ok((raw_tx, tx_hash))
    }
}
