// Copyright 2023 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use futures::stream::{Stream, StreamExt};
use inx::{client::InxClient, proto};
use iota_sdk::types::block::{self as iota, output::OutputId, slot::SlotIndex, BlockId};
use packable::PackableExt;

use super::{
    convert::TryConvertTo,
    ledger::{AcceptedTransaction, LedgerUpdate, UnspentOutput},
    request::SlotRangeRequest,
    responses::{Block, Output},
    InxError,
};
use crate::model::{
    block_metadata::{BlockMetadata, BlockWithMetadata},
    node::{NodeConfiguration, NodeStatus},
    raw::Raw,
    slot::Commitment,
};

/// An INX client connection.
#[derive(Clone, Debug)]
pub struct Inx {
    inx: InxClient<inx::tonic::transport::Channel>,
}

impl Inx {
    /// Connect to the INX interface of a node.
    pub async fn connect(address: &str) -> Result<Self, InxError> {
        Ok(Self {
            inx: InxClient::connect(address.to_owned()).await?,
        })
    }

    /// Get the status of the node.
    pub async fn get_node_status(&mut self) -> Result<NodeStatus, InxError> {
        self.inx.read_node_status(proto::NoParams {}).await?.try_convert()
    }

    /// Get the configuration of the node.
    pub async fn get_node_configuration(&mut self) -> Result<NodeConfiguration, InxError> {
        self.inx
            .read_node_configuration(proto::NoParams {})
            .await?
            .try_convert()
    }

    /// Get a commitment from a slot index.
    pub async fn get_commitment(&mut self, slot_index: SlotIndex) -> Result<Commitment, InxError> {
        self.inx
            .read_commitment(proto::CommitmentRequest {
                commitment_slot: slot_index.0,
                commitment_id: None,
            })
            .await?
            .try_convert()
    }

    /// Get a stream of committed slots.
    pub async fn get_committed_slots(
        &mut self,
        request: SlotRangeRequest,
    ) -> Result<impl Stream<Item = Result<Commitment, InxError>>, InxError> {
        Ok(self
            .inx
            .listen_to_commitments(proto::SlotRangeRequest::from(request))
            .await?
            .into_inner()
            .map(|msg| TryConvertTo::try_convert(msg?)))
    }

    /// Get a block using a block id.
    pub async fn get_block(&mut self, block_id: BlockId) -> Result<Raw<iota::Block>, InxError> {
        Ok(self
            .inx
            .read_block(proto::BlockId { id: block_id.to_vec() })
            .await?
            .into_inner()
            .try_into()?)
    }

    /// Get a block's metadata using a block id.
    pub async fn get_block_metadata(&mut self, block_id: BlockId) -> Result<BlockMetadata, InxError> {
        self.inx
            .read_block_metadata(proto::BlockId { id: block_id.to_vec() })
            .await?
            .try_convert()
    }

    /// Convenience wrapper that gets all blocks.
    pub async fn get_blocks(&mut self) -> Result<impl Stream<Item = Result<Block, InxError>>, InxError> {
        Ok(self
            .inx
            .listen_to_blocks(proto::NoParams {})
            .await?
            .into_inner()
            .map(|msg| TryConvertTo::try_convert(msg?)))
    }

    /// Convenience wrapper that gets accepted blocks.
    pub async fn get_accepted_blocks(
        &mut self,
    ) -> Result<impl Stream<Item = Result<BlockMetadata, InxError>>, InxError> {
        Ok(self
            .inx
            .listen_to_accepted_blocks(proto::NoParams {})
            .await?
            .into_inner()
            .map(|msg| TryConvertTo::try_convert(msg?)))
    }

    /// Convenience wrapper that gets confirmed blocks.
    pub async fn get_confirmed_blocks(
        &mut self,
    ) -> Result<impl Stream<Item = Result<BlockMetadata, InxError>>, InxError> {
        Ok(self
            .inx
            .listen_to_confirmed_blocks(proto::NoParams {})
            .await?
            .into_inner()
            .map(|msg| TryConvertTo::try_convert(msg?)))
    }

    /// Convenience wrapper that gets accepted blocks for a given slot.
    pub async fn get_accepted_blocks_for_slot(
        &mut self,
        slot_index: SlotIndex,
    ) -> Result<impl Stream<Item = Result<BlockWithMetadata, InxError>>, InxError> {
        Ok(self
            .inx
            .read_accepted_blocks(proto::SlotIndex { index: slot_index.0 })
            .await?
            .into_inner()
            .map(|msg| TryConvertTo::try_convert(msg?)))
    }

    /// Convenience wrapper that reads the current unspent outputs.
    pub async fn get_unspent_outputs(
        &mut self,
    ) -> Result<impl Stream<Item = Result<UnspentOutput, InxError>>, InxError> {
        Ok(self
            .inx
            .read_unspent_outputs(proto::NoParams {})
            .await?
            .into_inner()
            .map(|msg| TryConvertTo::try_convert(msg?)))
    }

    /// Convenience wrapper that listen to ledger updates.
    pub async fn get_ledger_updates(
        &mut self,
        request: SlotRangeRequest,
    ) -> Result<impl Stream<Item = Result<LedgerUpdate, InxError>>, InxError> {
        Ok(self
            .inx
            .listen_to_ledger_updates(proto::SlotRangeRequest::from(request))
            .await?
            .into_inner()
            .map(|msg| TryConvertTo::try_convert(msg?)))
    }

    /// Convenience wrapper that listen to accepted transactions.
    pub async fn get_accepted_transactions(
        &mut self,
    ) -> Result<impl Stream<Item = Result<AcceptedTransaction, InxError>>, InxError> {
        Ok(self
            .inx
            .listen_to_accepted_transactions(proto::NoParams {})
            .await?
            .into_inner()
            .map(|msg| TryConvertTo::try_convert(msg?)))
    }

    /// Get an output using an output id.
    pub async fn get_output(&mut self, output_id: OutputId) -> Result<Output, InxError> {
        self.inx
            .read_output(proto::OutputId {
                id: output_id.pack_to_vec(),
            })
            .await?
            .try_convert()
    }
}
