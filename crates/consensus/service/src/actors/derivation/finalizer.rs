//! The [`L2Finalizer`] for the derivation actor.

use std::collections::BTreeMap;

use alloy_primitives::B256;
use base_protocol::{AttributesWithParent, BlockInfo, L2BlockInfo};

/// An internal type alias for L1 block numbers.
type L1BlockNumber = u64;

/// An internal type alias for L2 block numbers.
type L2BlockNumber = u64;

/// A finalization candidate: the hash of the L1 block its L2 blocks were derived from, and the
/// highest L2 block number derived from that L1 block.
type Candidate = (B256, L2BlockNumber);

/// The [`L2Finalizer`] is responsible for tracking L2 blocks derived from L1 blocks and
/// determining which L2 blocks can be finalized when L1 blocks are finalized.
///
/// It maintains a queue of derived L2 blocks that are awaiting finalization, and returns
/// the L2 block numbers that can be finalized as new finalized L1 blocks are received.
///
/// Every candidate in `awaiting_finalization` was derived from an L1 block that the pipeline's
/// current L1 traversal passed. The traversal starts from the reset origin (fetched by number
/// from L1) and advances one parent-hash-checked block at a time, so that is the same chain the
/// finalized L1 signal is compared against. A derivation reset keeps this invariant: candidates
/// produced by the pre-reset traversal are held in `unverified` and only return to
/// `awaiting_finalization` once the post-reset traversal passes their L1 block with the same
/// hash.
#[derive(Debug, Default)]
pub struct L2Finalizer {
    /// A map of `L1 block number -> (L1 block hash, highest derived L2 block number)`, used to
    /// track derived [`AttributesWithParent`] awaiting finalization. When a new finalized L1
    /// block is received, the highest L2 block whose inputs are contained within the finalized
    /// L1 chain is finalized.
    awaiting_finalization: BTreeMap<L1BlockNumber, Candidate>,
    /// Candidates carried across a derivation reset whose L1 block the post-reset traversal has
    /// not passed yet. They are never finalized from here; see [`Self::observe_l1_origin`].
    unverified: BTreeMap<L1BlockNumber, Candidate>,
    /// The highest finalized L1 block observed by the finalizer.
    ///
    /// This survives derivation resets so that re-derived L2 blocks, and carried candidates the
    /// post-reset traversal re-verifies, can be finalized without waiting for the L1 watcher to
    /// emit the same finalized block again.
    finalized_l1_block: Option<BlockInfo>,
}

impl L2Finalizer {
    /// Enqueues a derived [`AttributesWithParent`] for finalization. When a new finalized L1
    /// block is observed that is `>=` the height of [`AttributesWithParent::derived_from`], the
    /// L2 block associated with the payload attributes will be finalized.
    pub fn enqueue_for_finalization(&mut self, attributes: &AttributesWithParent) {
        let derived_from = attributes
            .derived_from
            .expect("Fatal: Cannot enqueue attributes for finalization that weren't derived");
        let block_number = attributes.block_number();
        self.awaiting_finalization
            .entry(derived_from.number)
            .and_modify(|(hash, highest)| {
                if *hash == derived_from.hash {
                    *highest = (*highest).max(block_number);
                } else {
                    // The current traversal decides which L1 block sits at this height.
                    *hash = derived_from.hash;
                    *highest = block_number;
                }
            })
            .or_insert((derived_from.hash, block_number));
    }

    /// Records a finalized L1 block and attempts to finalize any eligible L2 blocks.
    ///
    /// Finalized L1 signals are monotonic by block number. Older signals are ignored, while a
    /// signal at the same height is accepted so that its block identity remains current.
    pub fn process_finalized_l1_block(
        &mut self,
        finalized_l1_block: BlockInfo,
    ) -> Option<L2BlockNumber> {
        if self
            .finalized_l1_block
            .as_ref()
            .is_some_and(|previous| finalized_l1_block.number < previous.number)
        {
            return None;
        }

        self.finalized_l1_block = Some(finalized_l1_block);
        self.try_finalize_pending()
    }

    /// Attempts to finalize an eligible L2 block using the latest finalized L1 signal.
    pub fn try_finalize_pending(&mut self) -> Option<L2BlockNumber> {
        self.try_finalize_next(self.finalized_l1_block?)
    }

    /// Handles a derivation reset to `safe_head`, preserving the finalized L1 signal.
    ///
    /// Candidates above the reset safe head are dropped: derivation re-derives those L2 blocks
    /// and enqueues them again. Candidates at or below it are kept as unverified, because the
    /// pre-reset traversal that produced them may have followed L1 blocks that were reorged out.
    /// [`Self::observe_l1_origin`] releases or drops them as the post-reset traversal advances.
    pub fn reset(&mut self, safe_head: &L2BlockInfo) {
        let safe = safe_head.block_info.number;
        let mut carried = std::mem::take(&mut self.awaiting_finalization);
        carried.append(&mut self.unverified);
        carried.retain(|_, (_, highest)| *highest <= safe);
        self.unverified = carried;
    }

    /// Records that the L1 traversal reached `origin`. Returns `true` when a carried candidate
    /// became finalizable again, so the caller can retry it against the retained finalized L1
    /// signal ([`Self::try_finalize_pending`]).
    ///
    /// The traversal reports every block it advances to, in order. A carried candidate at this
    /// height with the same hash was derived from a block on the chain the traversal follows now,
    /// so it is finalizable again, exactly like a freshly derived candidate. A different hash
    /// means the pre-reset traversal left that chain at or below this height: the candidate and
    /// every carried candidate above it were derived from the orphaned block or its descendants,
    /// so all of them are dropped. Carried candidates below this height were never matched and
    /// can no longer be verified; they are dropped too.
    pub fn observe_l1_origin(&mut self, origin: BlockInfo) -> bool {
        if self.unverified.is_empty() {
            return false;
        }
        self.unverified = self.unverified.split_off(&origin.number);
        let Some((hash, highest)) = self.unverified.remove(&origin.number) else {
            return false;
        };
        if hash != origin.hash {
            self.unverified.clear();
            return false;
        }
        self.awaiting_finalization.insert(origin.number, (hash, highest));
        true
    }

    /// Attempts to find L2 blocks that can be finalized based on the new finalized L1 block.
    ///
    /// Returns `Some(l2_block_number)` if there is an L2 block that can be finalized,
    /// or `None` if no blocks are ready for finalization.
    pub fn try_finalize_next(
        &mut self,
        new_finalized_l1_block: BlockInfo,
    ) -> Option<L2BlockNumber> {
        let finalized = new_finalized_l1_block.number;

        // Find the highest safe L2 block that is contained within the finalized chain,
        // that the finalizer is aware of.
        let highest_safe = self.awaiting_finalization.range(..=finalized).next_back();

        // If the highest safe block is found, return it and drain the
        // queue of all L1 blocks not contained in the finalized L1 chain.
        if let Some((_, (_, highest_safe_number))) = highest_safe {
            let result = *highest_safe_number;
            self.awaiting_finalization.retain(|&number, _| number > finalized);
            Some(result)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`L2Finalizer`] queue management.

    use alloy_eips::BlockNumHash;
    use alloy_primitives::B256;
    use base_common_rpc_types_engine::BasePayloadAttributes;
    use base_protocol::{AttributesWithParent, BlockInfo, L2BlockInfo};

    use super::L2Finalizer;

    /// Build a minimal [`AttributesWithParent`] whose derived L2 block number is
    /// `l2_parent_number + 1` and whose L1 origin is `l1_origin_number`.
    fn attrs(l2_parent_number: u64, l1_origin_number: u64) -> AttributesWithParent {
        let parent = L2BlockInfo {
            block_info: BlockInfo { number: l2_parent_number, ..Default::default() },
            l1_origin: BlockNumHash::default(),
            seq_num: 0,
        };
        let derived_from = BlockInfo { number: l1_origin_number, ..Default::default() };
        AttributesWithParent::new(
            BasePayloadAttributes::default(),
            parent,
            Some(derived_from),
            false,
        )
    }

    /// Build a [`BlockInfo`] representing a finalized L1 block at `number`.
    fn l1_at(number: u64) -> BlockInfo {
        BlockInfo { number, ..Default::default() }
    }

    /// An L1 block at `number` on `branch`. Equal arguments give equal hashes; different
    /// branches give different hashes at the same height.
    fn l1(number: u64, branch: u8) -> BlockInfo {
        let mut hash = B256::left_padding_from(&number.to_be_bytes());
        hash.0[0] = branch;
        BlockInfo { number, hash, ..Default::default() }
    }

    /// Attributes for L2 block `l2_number` derived from the L1 block `derived_from`.
    fn derived(l2_number: u64, derived_from: BlockInfo) -> AttributesWithParent {
        let parent = L2BlockInfo {
            block_info: BlockInfo { number: l2_number - 1, ..Default::default() },
            l1_origin: BlockNumHash::default(),
            seq_num: 0,
        };
        AttributesWithParent::new(
            BasePayloadAttributes::default(),
            parent,
            Some(derived_from),
            false,
        )
    }

    /// The L2 safe head a derivation reset rewinds to.
    fn safe_head(number: u64) -> L2BlockInfo {
        L2BlockInfo {
            block_info: BlockInfo { number, ..Default::default() },
            l1_origin: BlockNumHash::default(),
            seq_num: 0,
        }
    }

    #[test]
    fn empty_queue_returns_none() {
        let mut f = L2Finalizer::default();
        assert!(f.try_finalize_next(l1_at(100)).is_none());
    }

    #[test]
    fn finalized_l1_signal_is_retained_until_candidates_are_ready() {
        let mut f = L2Finalizer::default();
        assert!(f.process_finalized_l1_block(l1_at(5)).is_none());

        f.enqueue_for_finalization(&attrs(9, 5));

        assert_eq!(f.try_finalize_pending(), Some(10));
    }

    #[test]
    fn single_entry_l1_not_yet_finalized() {
        let mut f = L2Finalizer::default();
        // L2 block 10 came from L1 origin 5. Finalizing at L1=3 should not include it.
        f.enqueue_for_finalization(&attrs(9, 5)); // l2=10, l1_origin=5
        assert!(f.try_finalize_next(l1_at(3)).is_none());
        // Entry must still be in the queue.
        assert!(f.try_finalize_next(l1_at(5)).is_some());
    }

    #[test]
    fn single_entry_l1_exactly_at_finalized() {
        // Boundary: l1_origin == finalized_l1. The range is `..=`, so this must match.
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&attrs(9, 5)); // l2=10, l1_origin=5
        assert_eq!(f.try_finalize_next(l1_at(5)), Some(10));
    }

    #[test]
    fn multiple_l2_per_epoch_keeps_highest() {
        // Three L2 blocks all derived from L1 epoch 1. Only the highest (3) should be returned.
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&attrs(0, 1)); // l2=1, l1_origin=1
        f.enqueue_for_finalization(&attrs(1, 1)); // l2=2, l1_origin=1
        f.enqueue_for_finalization(&attrs(2, 1)); // l2=3, l1_origin=1
        assert_eq!(f.try_finalize_next(l1_at(1)), Some(3));
    }

    #[test]
    fn partial_finalization_drains_lower_entries() {
        // Entries at L1=1,2,3. Finalizing at L1=2 returns highest across L1<=2 and drains those.
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&attrs(4, 1)); // l2=5,  l1_origin=1
        f.enqueue_for_finalization(&attrs(7, 2)); // l2=8,  l1_origin=2
        f.enqueue_for_finalization(&attrs(10, 3)); // l2=11, l1_origin=3

        assert_eq!(f.try_finalize_next(l1_at(2)), Some(8));
        // L1=3 entry must still be present.
        assert_eq!(f.try_finalize_next(l1_at(3)), Some(11));
    }

    #[test]
    fn reset_holds_candidates_until_retraversal_reaches_them() {
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&derived(10, l1(5, 1)));
        f.reset(&safe_head(10));
        f.observe_l1_origin(l1(4, 1));
        // The post-reset traversal has not reached height 5 yet.
        assert!(f.try_finalize_next(l1(5, 1)).is_none());
    }

    #[test]
    fn carried_candidate_is_released_when_retraversal_matches_its_hash() {
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&derived(10, l1(5, 1)));
        f.reset(&safe_head(10));
        f.observe_l1_origin(l1(4, 1));
        f.observe_l1_origin(l1(5, 1));
        assert_eq!(f.try_finalize_next(l1(5, 1)), Some(10));
    }

    #[test]
    fn reset_drops_candidates_above_reset_safe_head() {
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&derived(10, l1(5, 1)));
        f.enqueue_for_finalization(&derived(20, l1(6, 1)));
        f.reset(&safe_head(10));
        f.observe_l1_origin(l1(5, 1));
        f.observe_l1_origin(l1(6, 1));
        assert_eq!(f.try_finalize_next(l1(6, 1)), Some(10));
        assert!(f.try_finalize_next(l1(100, 1)).is_none());
    }

    #[test]
    fn reorged_inclusion_block_and_candidates_above_it_are_never_finalized() {
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&derived(10, l1(5, 1)));
        f.enqueue_for_finalization(&derived(20, l1(6, 1)));
        f.enqueue_for_finalization(&derived(30, l1(7, 1)));
        f.reset(&safe_head(30));
        f.observe_l1_origin(l1(5, 1));
        // L1 reorged at height 6: the post-reset traversal follows branch 2 from here on.
        f.observe_l1_origin(l1(6, 2));
        f.observe_l1_origin(l1(7, 2));
        assert_eq!(f.try_finalize_next(l1(7, 2)), Some(10));
        assert!(f.try_finalize_next(l1(100, 2)).is_none());
    }

    #[test]
    fn carried_candidates_below_the_retraversal_start_are_dropped() {
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&derived(10, l1(5, 1)));
        f.enqueue_for_finalization(&derived(20, l1(8, 1)));
        f.reset(&safe_head(20));
        // The reset walked the origin back to height 7, so height 5 is never re-observed.
        f.observe_l1_origin(l1(7, 1));
        assert!(f.try_finalize_next(l1(6, 1)).is_none());
        f.observe_l1_origin(l1(8, 1));
        assert_eq!(f.try_finalize_next(l1(8, 1)), Some(20));
    }

    #[test]
    fn candidates_derived_after_reset_finalize_normally() {
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&derived(10, l1(5, 1)));
        f.reset(&safe_head(10));
        f.observe_l1_origin(l1(5, 1));
        f.enqueue_for_finalization(&derived(11, l1(5, 1)));
        f.observe_l1_origin(l1(6, 1));
        f.enqueue_for_finalization(&derived(12, l1(6, 1)));
        assert_eq!(f.try_finalize_next(l1(5, 1)), Some(11));
        assert_eq!(f.try_finalize_next(l1(6, 1)), Some(12));
    }

    #[test]
    fn reset_preserves_finalized_l1_signal() {
        let mut f = L2Finalizer::default();
        f.process_finalized_l1_block(l1_at(5));
        f.enqueue_for_finalization(&attrs(4, 1));

        // The reset rewinds below L2 block 5, so its candidate is dropped and re-derived.
        f.reset(&safe_head(4));
        f.enqueue_for_finalization(&attrs(9, 5));

        assert_eq!(f.try_finalize_pending(), Some(10));
    }

    #[test]
    fn drain_preserves_future_entries() {
        // After finalizing up to L1=2, entries at L1=5 must survive.
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&attrs(4, 2)); // l2=5,  l1_origin=2
        f.enqueue_for_finalization(&attrs(9, 5)); // l2=10, l1_origin=5

        assert_eq!(f.try_finalize_next(l1_at(2)), Some(5));
        // L1=5 entry is still present; finalizing it now returns l2=10.
        assert_eq!(f.try_finalize_next(l1_at(5)), Some(10));
    }

    #[test]
    fn old_finalized_signal_returns_none_after_drain() {
        // After draining all entries up to L1=5, a later signal for L1=2 finds nothing.
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&attrs(19, 5)); // l2=20, l1_origin=5

        assert_eq!(f.try_finalize_next(l1_at(5)), Some(20));
        // Queue is now empty; an older signal cannot regress to a stale entry.
        assert!(f.try_finalize_next(l1_at(2)).is_none());
    }

    #[test]
    fn older_finalized_signal_does_not_lower_high_water_mark() {
        let mut f = L2Finalizer::default();
        f.process_finalized_l1_block(l1_at(5));
        assert!(f.process_finalized_l1_block(l1_at(3)).is_none());

        f.enqueue_for_finalization(&attrs(9, 4));

        assert_eq!(f.try_finalize_pending(), Some(10));
    }

    #[test]
    fn reverified_candidates_finalize_against_the_retained_signal() {
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&derived(10, l1(5, 1)));
        f.enqueue_for_finalization(&derived(20, l1(7, 1)));
        f.reset(&safe_head(20));
        // The finalized signal arrives while every candidate is parked: nothing is finalized.
        assert!(f.process_finalized_l1_block(l1(7, 1)).is_none());

        assert!(f.observe_l1_origin(l1(5, 1)));
        assert_eq!(f.try_finalize_pending(), Some(10));
        assert!(!f.observe_l1_origin(l1(6, 1)));
        assert!(f.try_finalize_pending().is_none());
        assert!(f.observe_l1_origin(l1(7, 1)));
        assert_eq!(f.try_finalize_pending(), Some(20));
    }

    #[test]
    fn retained_signal_never_finalizes_a_candidate_from_an_orphaned_block() {
        let mut f = L2Finalizer::default();
        f.enqueue_for_finalization(&derived(10, l1(5, 1)));
        f.enqueue_for_finalization(&derived(20, l1(6, 1)));
        f.reset(&safe_head(20));
        assert!(f.process_finalized_l1_block(l1(6, 2)).is_none());

        assert!(f.observe_l1_origin(l1(5, 1)));
        assert_eq!(f.try_finalize_pending(), Some(10));
        // L1 reorged at height 6: the candidate derived from the orphaned block is dropped.
        assert!(!f.observe_l1_origin(l1(6, 2)));
        assert!(f.try_finalize_pending().is_none());
    }

    /// Replays the Base mainnet L1 reorg of 2026-09-30: L1 block 26090572 was orphaned after
    /// L2 blocks 51992616..=51992630 had been derived from it, and derivation reset to safe head
    /// 51992630. Candidates (L1 inclusion block -> highest L2 block) are the ones the writer's
    /// logs show before the reset. Before this change the reset discarded all of them, so the
    /// writer skipped the finalization of 51992615 that other nodes applied and stayed at
    /// 51992245 until 51992815 finalized.
    #[test]
    fn l1_reorg_at_26090572_keeps_candidates_below_the_fork() {
        const CANONICAL: u8 = 1;
        const ORPHANED: u8 = 2;
        let mut f = L2Finalizer::default();
        for (l1_number, l2_number) in [
            (26_090_554, 51_992_520),
            (26_090_556, 51_992_533),
            (26_090_559, 51_992_549),
            (26_090_561, 51_992_566),
            (26_090_564, 51_992_583),
            (26_090_567, 51_992_601),
            (26_090_570, 51_992_615),
        ] {
            f.enqueue_for_finalization(&derived(l2_number, l1(l1_number, CANONICAL)));
        }
        f.enqueue_for_finalization(&derived(51_992_630, l1(26_090_572, ORPHANED)));

        f.reset(&safe_head(51_992_630));
        for l1_number in 26_090_506..=26_090_572 {
            f.observe_l1_origin(l1(l1_number, CANONICAL));
        }
        f.enqueue_for_finalization(&derived(51_992_815, l1(26_090_603, CANONICAL)));

        assert_eq!(f.try_finalize_next(l1(26_090_556, CANONICAL)), Some(51_992_533));
        assert_eq!(f.try_finalize_next(l1(26_090_570, CANONICAL)), Some(51_992_615));
        // Nothing derived from the orphaned block is ever finalized.
        assert!(f.try_finalize_next(l1(26_090_572, CANONICAL)).is_none());
        assert_eq!(f.try_finalize_next(l1(26_090_603, CANONICAL)), Some(51_992_815));
    }
}
