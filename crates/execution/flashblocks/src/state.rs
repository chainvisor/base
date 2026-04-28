//! Flashblocks state management.

use std::sync::Arc;

use alloy_consensus::Header;
use arc_swap::{ArcSwapOption, Guard};
use base_common_chains::Upgrades;
use base_common_consensus::BaseBlock;
use base_common_flashblocks::Flashblock;
use reth_chainspec::{ChainSpecProvider, EthChainSpec};
use reth_primitives::RecoveredBlock;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use tokio::sync::{
    Mutex,
    broadcast::{self, Sender},
    mpsc,
};

use crate::{
    FlashblocksAPI, FlashblocksReceiver, PendingBlocks,
    metrics::Metrics,
    processor::{StateProcessor, StateUpdate},
};

// Buffer 4s of flashblocks for flashblock_sender
const BUFFER_SIZE: usize = 20;

// Unified bounded processing queue. Flashblocks drop on saturation; canonical
// blocks apply backpressure instead of being dropped.
const FLASHBLOCK_QUEUE_CAPACITY: usize = 1024;

/// Manages the pending flashblock state and processes incoming updates.
#[derive(Debug)]
pub struct FlashblocksState {
    pending_blocks: Arc<ArcSwapOption<PendingBlocks>>,
    queue: mpsc::Sender<StateUpdate>,
    rx: Arc<Mutex<mpsc::Receiver<StateUpdate>>>,
    flashblock_sender: Sender<Arc<PendingBlocks>>,
    max_pending_blocks_depth: u64,
}

impl FlashblocksState {
    /// Creates a new flashblocks state manager.
    ///
    /// The state is created without a client. Call [`start`](Self::start) with a client
    /// to spawn the state processor after the node is launched.
    pub fn new(max_pending_blocks_depth: u64) -> Self {
        let (queue, rx) = mpsc::channel::<StateUpdate>(FLASHBLOCK_QUEUE_CAPACITY);
        let pending_blocks: Arc<ArcSwapOption<PendingBlocks>> = Arc::new(ArcSwapOption::new(None));
        let (flashblock_sender, _) = broadcast::channel(BUFFER_SIZE);

        Self {
            pending_blocks,
            queue,
            rx: Arc::new(Mutex::new(rx)),
            flashblock_sender,
            max_pending_blocks_depth,
        }
    }

    /// Starts the flashblocks state processor with the given client.
    ///
    /// This spawns a background task that processes canonical blocks and flashblocks.
    /// Should be called after the node is launched and the provider is available.
    pub fn start<Client>(&self, client: Client)
    where
        Client: StateProviderFactory
            + ChainSpecProvider<ChainSpec: EthChainSpec<Header = Header> + Upgrades>
            + BlockReaderIdExt<Header = Header>
            + Clone
            + 'static,
    {
        let state_processor = StateProcessor::new(
            client,
            Arc::clone(&self.pending_blocks),
            self.max_pending_blocks_depth,
            Arc::clone(&self.rx),
            self.flashblock_sender.clone(),
        );

        tokio::spawn(async move {
            state_processor.start().await;
        });
    }

    /// Handles a canonical block by waiting for queue capacity instead of dropping it.
    pub async fn on_canonical_block_received(&self, block: RecoveredBlock<BaseBlock>) {
        let block_number = block.number;
        match self.queue.send(StateUpdate::Canonical(block)).await {
            Ok(_) => {
                info!(message = "added canonical block to processing queue", block_number)
            }
            Err(e) => {
                error!(message = "could not add canonical block to processing queue", block_number, error = %e);
            }
        }
    }
}

impl FlashblocksReceiver for FlashblocksState {
    fn on_flashblock_received(&self, flashblock: Flashblock) {
        let flashblock_index = flashblock.index;
        let block_number = flashblock.metadata.block_number;
        match self.queue.try_send(StateUpdate::Flashblock(flashblock)) {
            Ok(_) => {
                debug!(
                    message = "added flashblock to processing queue",
                    block_number, flashblock_index,
                );
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                Metrics::flashblock_queue_drops().increment(1);
                warn!(
                    message = "dropped flashblock because processing queue is full",
                    block_number,
                    flashblock_index,
                    capacity = FLASHBLOCK_QUEUE_CAPACITY,
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                error!(
                    message = "could not add flashblock to processing queue: receiver closed",
                    block_number,
                    flashblock_index,
                );
            }
        }
    }
}

impl Default for FlashblocksState {
    fn default() -> Self {
        Self::new(10)
    }
}

impl FlashblocksAPI for FlashblocksState {
    fn get_pending_blocks(&self) -> Guard<Option<Arc<PendingBlocks>>> {
        self.pending_blocks.load()
    }

    fn subscribe_to_flashblocks(&self) -> broadcast::Receiver<Arc<PendingBlocks>> {
        self.flashblock_sender.subscribe()
    }
}

impl FlashblocksState {
    /// Sets the pending blocks directly for testing purposes.
    ///
    /// This bypasses the normal flashblock processing pipeline and allows
    /// tests to inject a pre-built `PendingBlocks` state.
    pub fn set_pending_blocks_for_testing(&self, pending_blocks: Option<PendingBlocks>) {
        self.pending_blocks.store(pending_blocks.map(Arc::new));
    }
}
