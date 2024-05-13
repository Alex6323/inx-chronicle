// Copyright 2023 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{stream::BoxStream, Stream, TryStreamExt};
use iota_sdk::types::{
    api::core::BlockState,
    block::slot::{SlotCommitment, SlotCommitmentId, SlotIndex},
};

use super::InputSource;
use crate::model::{
    block_metadata::BlockWithTransactionMetadata, ledger::LedgerUpdateStore, raw::Raw, slot::Commitment,
};

#[allow(missing_docs)]
pub struct Slot<'a, I: InputSource> {
    pub(super) source: &'a I,
    pub commitment: Commitment,
    pub ledger_updates: LedgerUpdateStore,
}

impl<'a, I: InputSource> Slot<'a, I> {
    /// Get the slot's index.
    pub fn index(&self) -> SlotIndex {
        self.commitment.commitment_id.slot_index()
    }

    /// Get the slot's commitment id.
    pub fn commitment_id(&self) -> SlotCommitmentId {
        self.commitment.commitment_id
    }

    /// Get the slot's raw commitment.
    pub fn commitment(&self) -> &Raw<SlotCommitment> {
        &self.commitment.commitment
    }

    /// Check whether the slot has been finalized.
    pub async fn is_finalized(&self) -> bool {
        // TODO: unwrap
        self.commitment.commitment_id.slot_index() >= self.source.latest_finalized_slot_index().await.unwrap()
    }
}

impl<'a, I: InputSource> Slot<'a, I> {
    /// Returns the accepted blocks of a slot.
    pub async fn finalized_block_stream(
        &self,
    ) -> Result<impl Stream<Item = Result<BlockWithTransactionMetadata, I::Error>> + '_, I::Error> {
        while !self.is_finalized().await {
            println!("not finalized: {}", self.index());
            tokio::time::sleep(core::time::Duration::from_millis(100)).await;
        }
        println!("finalized: {}", self.index());

        Ok(self
            .source
            .accepted_blocks(self.index())
            .await?
            .try_filter(|block_with_metadata| {
                futures::future::ready(block_with_metadata.metadata.block_state == Some(BlockState::Finalized))
            })
            .and_then(|res| async {
                let transaction = if let Some(transaction_id) = res
                    .block
                    .inner()
                    .body()
                    .as_basic_opt()
                    .and_then(|body| body.payload())
                    .and_then(|p| p.as_signed_transaction_opt())
                    .map(|txn| txn.transaction().id())
                {
                    Some(self.source.transaction_metadata(transaction_id).await?)
                } else {
                    None
                };
                Ok(BlockWithTransactionMetadata {
                    transaction,
                    block: res,
                })
            }))
    }

    /// Returns the ledger update store.
    pub fn ledger_updates(&self) -> &LedgerUpdateStore {
        &self.ledger_updates
    }
}

#[allow(missing_docs)]
pub struct SlotStream<'a, I: InputSource> {
    pub(super) inner: BoxStream<'a, Result<Slot<'a, I>, I::Error>>,
}

impl<'a, I: InputSource> Stream for SlotStream<'a, I> {
    type Item = Result<Slot<'a, I>, I::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().inner).poll_next(cx)
    }
}
