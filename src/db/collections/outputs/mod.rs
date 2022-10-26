// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

mod indexer;

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
};

use decimal::d128;
use futures::TryStreamExt;
use mongodb::{
    bson::{doc, to_bson, to_document},
    error::Error,
    options::{IndexOptions, InsertManyOptions},
    IndexModel,
};
use serde::{Deserialize, Serialize};
use tracing::instrument;

pub use self::indexer::{
    AliasOutputsQuery, BasicOutputsQuery, FoundryOutputsQuery, IndexedId, NftOutputsQuery, OutputsResult,
};
use super::analytics::{
    AddressActivityAnalytics, AddressAnalytics, AliasActivityAnalytics, BaseTokenActivityAnalytics,
    ClaimedTokensAnalytics, FoundryActivityAnalytics, LedgerOutputAnalytics, LedgerSizeAnalytics, NftActivityAnalytics,
    UnlockConditionAnalytics,
};
use crate::{
    db::{
        mongodb::{InsertIgnoreDuplicatesExt, MongoDbCollection, MongoDbCollectionExt},
        MongoDb,
    },
    types::{
        ledger::{
            LedgerOutput, LedgerSpent, MilestoneIndexTimestamp, OutputMetadata, RentStructureBytes, SpentMetadata,
        },
        stardust::block::{
            output::{
                AliasId, AliasOutput, BasicOutput, FoundryId, FoundryOutput, NftId, NftOutput, Output, OutputId,
                TreasuryOutput,
            },
            Address, BlockId,
        },
        tangle::MilestoneIndex,
    },
};

/// Chronicle Output record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputDocument {
    #[serde(rename = "_id")]
    output_id: OutputId,
    output: Output,
    metadata: OutputMetadata,
    details: OutputDetails,
}

/// The stardust outputs collection.
pub struct OutputCollection {
    db: mongodb::Database,
    collection: mongodb::Collection<OutputDocument>,
}

#[async_trait::async_trait]
impl MongoDbCollection for OutputCollection {
    const NAME: &'static str = "stardust_outputs";
    type Document = OutputDocument;

    fn instantiate(db: &MongoDb, collection: mongodb::Collection<Self::Document>) -> Self {
        Self {
            db: db.db.clone(),
            collection,
        }
    }

    fn collection(&self) -> &mongodb::Collection<Self::Document> {
        &self.collection
    }

    async fn create_indexes(&self) -> Result<(), Error> {
        self.create_index(
            IndexModel::builder()
                .keys(doc! { "details.address": 1 })
                .options(
                    IndexOptions::builder()
                        .unique(false)
                        .name("address_index".to_string())
                        .partial_filter_expression(doc! {
                            "details.address": { "$exists": true },
                        })
                        .build(),
                )
                .build(),
            None,
        )
        .await?;

        self.create_index(
            IndexModel::builder()
                .keys(doc! { "metadata.block_id": 1 })
                .options(
                    IndexOptions::builder()
                        .unique(false)
                        .name("metadata_block_id".to_string())
                        .build(),
                )
                .build(),
            None,
        )
        .await?;

        self.create_indexer_indexes().await?;

        Ok(())
    }
}

/// Precalculated info and other output details.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<Address>,
    pub is_trivial_unlock: bool,
    pub rent_structure: RentStructureBytes,
}

impl From<&LedgerOutput> for OutputDocument {
    fn from(rec: &LedgerOutput) -> Self {
        let address = rec.output.owning_address().copied();
        let is_trivial_unlock = rec.output.is_trivial_unlock();

        Self {
            output_id: rec.output_id,
            output: rec.output.clone(),
            metadata: OutputMetadata {
                block_id: rec.block_id,
                booked: rec.booked,
                spent_metadata: None,
            },
            details: OutputDetails {
                address,
                is_trivial_unlock,
                rent_structure: rec.rent_structure,
            },
        }
    }
}

impl From<&LedgerSpent> for OutputDocument {
    fn from(rec: &LedgerSpent) -> Self {
        let mut res = Self::from(&rec.output);
        res.metadata.spent_metadata.replace(rec.spent_metadata);
        res
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[allow(missing_docs)]
pub struct OutputMetadataResult {
    pub output_id: OutputId,
    pub block_id: BlockId,
    pub booked: MilestoneIndexTimestamp,
    pub spent_metadata: Option<SpentMetadata>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[allow(missing_docs)]
pub struct OutputWithMetadataResult {
    pub output: Output,
    pub metadata: OutputMetadataResult,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(missing_docs)]
pub struct BalanceResult {
    pub total_balance: String,
    pub sig_locked_balance: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[allow(missing_docs)]
pub struct UtxoChangesResult {
    pub created_outputs: Vec<OutputId>,
    pub consumed_outputs: Vec<OutputId>,
}

/// Implements the queries for the core API.
impl OutputCollection {
    /// Upserts [`Outputs`](crate::types::stardust::block::Output) with their
    /// [`OutputMetadata`](crate::types::ledger::OutputMetadata).
    #[instrument(skip_all, err, level = "trace")]
    pub async fn update_spent_outputs(&self, outputs: impl IntoIterator<Item = &LedgerSpent>) -> Result<(), Error> {
        // TODO: Replace `db.run_command` once the `BulkWrite` API lands in the Rust driver.
        let update_docs = outputs
            .into_iter()
            .map(|output| {
                Ok(doc! {
                    "q": { "_id": output.output.output_id },
                    "u": to_document(&OutputDocument::from(output))?,
                    "upsert": true,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        if !update_docs.is_empty() {
            let mut command = doc! {
                "update": Self::NAME,
                "updates": update_docs,
            };
            if let Some(ref write_concern) = self.db.write_concern() {
                command.insert("writeConcern", to_bson(write_concern)?);
            }
            let selection_criteria = self.db.selection_criteria().cloned();
            let _ = self.db.run_command(command, selection_criteria).await?;
        }

        Ok(())
    }

    /// Inserts [`Outputs`](crate::types::stardust::block::Output) with their
    /// [`OutputMetadata`](crate::types::ledger::OutputMetadata).
    #[instrument(skip_all, err, level = "trace")]
    pub async fn insert_unspent_outputs<I, B>(&self, outputs: I) -> Result<(), Error>
    where
        I: IntoIterator<Item = B>,
        I::IntoIter: Send + Sync,
        B: Borrow<LedgerOutput>,
    {
        self.insert_many_ignore_duplicates(
            outputs.into_iter().map(|d| OutputDocument::from(d.borrow())),
            InsertManyOptions::builder().ordered(false).build(),
        )
        .await?;

        Ok(())
    }

    /// Get an [`Output`] by [`OutputId`].
    pub async fn get_output(&self, output_id: &OutputId) -> Result<Option<Output>, Error> {
        self.aggregate(
            vec![
                doc! { "$match": { "_id": output_id } },
                doc! { "$replaceWith": "$output" },
            ],
            None,
        )
        .await?
        .try_next()
        .await
    }

    /// Get an [`Output`] with its [`OutputMetadata`] by [`OutputId`].
    pub async fn get_output_with_metadata(
        &self,
        output_id: &OutputId,
        ledger_index: MilestoneIndex,
    ) -> Result<Option<OutputWithMetadataResult>, Error> {
        self.aggregate(
            vec![
                doc! { "$match": {
                    "_id": output_id,
                    "metadata.booked.milestone_index": { "$lte": ledger_index }
                } },
                doc! { "$project": {
                    "output": "$output",
                    "metadata": {
                        "output_id": "$_id",
                        "block_id": "$metadata.block_id",
                        "booked": "$metadata.booked",
                        "spent_metadata": "$metadata.spent_metadata",
                    },
                } },
            ],
            None,
        )
        .await?
        .try_next()
        .await
    }

    /// Get an [`OutputMetadata`] by [`OutputId`].
    pub async fn get_output_metadata(
        &self,
        output_id: &OutputId,
        ledger_index: MilestoneIndex,
    ) -> Result<Option<OutputMetadataResult>, Error> {
        self.aggregate(
            vec![
                doc! { "$match": {
                    "_id": &output_id,
                    "metadata.booked.milestone_index": { "$lte": ledger_index }
                } },
                doc! { "$project": {
                    "output_id": "$_id",
                    "block_id": "$metadata.block_id",
                    "booked": "$metadata.booked",
                    "spent_metadata": "$metadata.spent_metadata",
                } },
            ],
            None,
        )
        .await?
        .try_next()
        .await
    }

    /// Gets the spending transaction metadata of an [`Output`] by [`OutputId`].
    pub async fn get_spending_transaction_metadata(
        &self,
        output_id: &OutputId,
    ) -> Result<Option<SpentMetadata>, Error> {
        self.aggregate(
            vec![
                doc! { "$match": {
                    "_id": &output_id,
                    "metadata.spent_metadata": { "$ne": null }
                } },
                doc! { "$replaceWith": "$metadata.spent_metadata" },
            ],
            None,
        )
        .await?
        .try_next()
        .await
    }

    /// Sums the amounts of all outputs owned by the given [`Address`](crate::types::stardust::block::Address).
    pub async fn get_address_balance(
        &self,
        address: Address,
        ledger_index: MilestoneIndex,
    ) -> Result<Option<BalanceResult>, Error> {
        self
            .aggregate(
                vec![
                    // Look at all (at ledger index o'clock) unspent output documents for the given address.
                    doc! { "$match": {
                        "details.address": &address,
                        "metadata.booked.milestone_index": { "$lte": ledger_index },
                        "$or": [
                            { "metadata.spent_metadata.spent": null },
                            { "metadata.spent_metadata.spent.milestone_index": { "$gt": ledger_index } },
                        ]
                    } },
                    doc! { "$group": {
                        "_id": null,
                        "total_balance": { "$sum": { "$toDecimal": "$output.amount" } },
                        "sig_locked_balance": { "$sum": { 
                            "$cond": [ { "$eq": [ "$details.is_trivial_unlock", true] }, { "$toDecimal": "$output.amount" }, 0 ]
                        } },
                    } },
                    doc! { "$project": {
                        "total_balance": { "$toString": "$total_balance" },
                        "sig_locked_balance": { "$toString": "$sig_locked_balance" },
                    } },
                ],
                None,
            )
            .await?
            .try_next()
            .await
    }

    /// Returns the changes to the UTXO ledger (as consumed and created output ids) that were applied at the given
    /// `index`. It returns `None` if the provided `index` is out of bounds (beyond Chronicle's ledger index). If
    /// the associated milestone did not perform any changes to the ledger, the returned `Vec`s will be empty.
    pub async fn get_utxo_changes(
        &self,
        index: MilestoneIndex,
        ledger_index: MilestoneIndex,
    ) -> Result<Option<UtxoChangesResult>, Error> {
        if index > ledger_index {
            Ok(None)
        } else {
            Ok(Some(
                self.aggregate(
                    vec![doc! { "$facet": {
                        "created_outputs": [
                            { "$match": { "metadata.booked.milestone_index": index  } },
                            { "$replaceWith": "$_id" },
                        ],
                        "consumed_outputs": [
                            { "$match": { "metadata.spent_metadata.spent.milestone_index": index } },
                            { "$replaceWith": "$_id" },
                        ],
                    } }],
                    None,
                )
                .await?
                .try_next()
                .await?
                .unwrap_or_default(),
            ))
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OutputAnalyticsResult {
    pub count: u64,
    pub total_value: String,
}

impl OutputCollection {
    /// Gathers output analytics.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_base_token_activity_analytics(
        &self,
        milestone_index: MilestoneIndex,
    ) -> Result<BaseTokenActivityAnalytics, Error> {
        Ok(self
            .aggregate(
                vec![
                    doc! { "$match": {
                        "metadata.booked.milestone_index": milestone_index,
                    } },
                    doc! { "$group" : {
                        "_id": null,
                        "transferred_value": { "$sum": { "$toDecimal": "$output.amount" } },
                    } },
                    doc! { "$project": {
                        "transferred_value": { "$toString": "$transferred_value" },
                    } },
                ],
                None,
            )
            .await?
            .try_next()
            .await?
            .unwrap_or_default())
    }

    /// Gathers ledger (unspent) output analytics.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_ledger_output_analytics(
        &self,
        ledger_index: MilestoneIndex,
    ) -> Result<LedgerOutputAnalytics, Error> {
        #[derive(Default, Deserialize)]
        struct Res {
            count: u64,
            value: d128,
        }

        let query = |kind: &'static str| async move {
            Result::<_, Error>::Ok(
                self.aggregate::<Res>(
                    vec![
                        doc! { "$match": {
                            "output.kind": kind,
                            "metadata.booked.milestone_index": { "$lte": ledger_index },
                            "$or": [
                                { "metadata.spent_metadata.spent": null },
                                { "metadata.spent_metadata.spent.milestone_index": { "$gt": ledger_index } },
                            ],
                        } },
                        doc! { "$group" : {
                            "_id": null,
                            "count": { "$sum": 1 },
                            "value": { "$sum": { "$toDecimal": "$output.amount" } },
                        } },
                        doc! { "$project": {
                            "count": 1,
                            "value": { "$toString": "$value" },
                        } },
                    ],
                    None,
                )
                .await?
                .try_next()
                .await?
                .unwrap_or_default(),
            )
        };

        let (basic, alias, foundry, nft, treasury) = tokio::try_join!(
            query(BasicOutput::KIND),
            query(AliasOutput::KIND),
            query(FoundryOutput::KIND),
            query(NftOutput::KIND),
            query(TreasuryOutput::KIND)
        )?;

        Ok(LedgerOutputAnalytics {
            basic_count: basic.count,
            basic_value: basic.value,
            alias_count: alias.count,
            alias_value: alias.value,
            foundry_count: foundry.count,
            foundry_value: foundry.value,
            nft_count: nft.count,
            nft_value: nft.value,
            treasury_count: treasury.count,
            treasury_value: treasury.value,
        })
    }
}

impl OutputCollection {
    pub(crate) async fn get_unique_nft_ids(&self, index: MilestoneIndex) -> Result<HashSet<NftId>, Error> {
        #[derive(Deserialize)]
        struct NftIdsResult {
            id: NftId,
        }

        self.aggregate::<NftIdsResult>(
            vec![
                doc! { "$match": {
                    "output.kind": "nft",
                    "metadata.booked.milestone_index": { "$lte": index },
                    "$or": [
                        { "metadata.spent_metadata.spent": null },
                        { "metadata.spent_metadata.spent.milestone_index": { "$gt": index } },
                    ],
                } },
                doc! { "$project": {
                    "id": "$output.nft_id"
                } },
            ],
            None,
        )
        .await?
        .map_ok(|res| res.id)
        .try_collect()
        .await
    }

    /// Gathers unique nft ids that were created/transferred/burned in the given milestone.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_nft_output_analytics(&self, index: MilestoneIndex) -> Result<NftActivityAnalytics, Error> {
        if index == 0 {
            return Ok(Default::default());
        }
        let (start_state, end_state) =
            tokio::try_join!(self.get_unique_nft_ids(index - 1), self.get_unique_nft_ids(index))?;

        Ok(NftActivityAnalytics {
            created_count: end_state.difference(&start_state).copied().count() as _,
            transferred_count: start_state.intersection(&end_state).copied().count() as _,
            destroyed_count: start_state.difference(&end_state).copied().count() as _,
        })
    }

    pub(crate) async fn get_unique_foundry_ids(&self, index: MilestoneIndex) -> Result<HashSet<FoundryId>, Error> {
        #[derive(Deserialize)]
        struct FoundryIdsResult {
            id: FoundryId,
        }

        self.aggregate::<FoundryIdsResult>(
            vec![
                doc! { "$match": {
                    "output.kind": "foundry",
                    "metadata.booked.milestone_index": { "$lte": index },
                    "$or": [
                        { "metadata.spent_metadata.spent": null },
                        { "metadata.spent_metadata.spent.milestone_index": { "$gt": index } },
                    ],
                } },
                doc! { "$project": {
                    "id": "$output.foundry_id"
                } },
            ],
            None,
        )
        .await?
        .map_ok(|res| res.id)
        .try_collect()
        .await
    }

    /// Gathers unique foundry ids that were created/transferred/burned in the given milestone.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_foundry_output_analytics(&self, index: MilestoneIndex) -> Result<FoundryActivityAnalytics, Error> {
        if index == 0 {
            return Ok(Default::default());
        }
        let (start_state, end_state) = tokio::try_join!(
            self.get_unique_foundry_ids(index - 1),
            self.get_unique_foundry_ids(index)
        )?;
        Ok(FoundryActivityAnalytics {
            created_count: end_state.difference(&start_state).copied().count() as _,
            transferred_count: start_state.intersection(&end_state).copied().count() as _,
            destroyed_count: start_state.difference(&end_state).copied().count() as _,
        })
    }

    /// Gathers unique foundry ids that were created/transferred/burned in the given milestone.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_alias_output_tracker(&self, index: MilestoneIndex) -> Result<AliasActivityAnalytics, Error> {
        if index == 0 {
            return Ok(Default::default());
        }
        let (start_state, end_state) =
            tokio::try_join!(self.get_unique_alias_ids(index - 1), self.get_unique_alias_ids(index))?;

        Ok(AliasActivityAnalytics {
            created_count: end_state
                .iter()
                .filter_map(|(end_id, end_state)| {
                    if start_state.get(end_id).is_some() {
                        None
                    } else {
                        Some((*end_id, *end_state))
                    }
                })
                .count() as _,
            governor_changed_count: start_state
                .iter()
                .filter_map(|(start_id, start_state)| {
                    if let Some(end_state) = end_state.get(start_id) {
                        if end_state == start_state {
                            Some((*start_id, *end_state))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .count() as _,
            state_changed_count: start_state
                .iter()
                .filter_map(|(start_id, start_state)| {
                    if let Some(end_state) = end_state.get(start_id) {
                        if end_state != start_state {
                            Some((*start_id, *end_state))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .count() as _,
            destroyed_count: start_state
                .iter()
                .filter_map(|(start_id, start_state)| {
                    if end_state.get(start_id).is_some() {
                        None
                    } else {
                        Some((*start_id, *start_state))
                    }
                })
                .count() as _,
        })
    }

    pub(crate) async fn get_unique_alias_ids(&self, index: MilestoneIndex) -> Result<HashMap<AliasId, u32>, Error> {
        #[derive(Deserialize)]
        struct AliasIdsResult {
            id: AliasId,
            state_index: u32,
        }

        self.aggregate::<AliasIdsResult>(
            vec![
                doc! { "$match": {
                    "output.kind": "alias",
                    "metadata.booked.milestone_index": { "$lte": index },
                    "$or": [
                        { "metadata.spent_metadata.spent": null },
                        { "metadata.spent_metadata.spent.milestone_index": { "$gt": index } },
                    ],
                } },
                doc! { "$project": {
                    "id": "$output.alias_id",
                    "state_index": "$output.state_index"
                } },
            ],
            None,
        )
        .await?
        .map_ok(|res| (res.id, res.state_index))
        .try_collect()
        .await
    }
}

impl OutputCollection {
    /// Gathers byte cost and storage deposit analytics.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_ledger_size_analytics(&self, ledger_index: MilestoneIndex) -> Result<LedgerSizeAnalytics, Error> {
        Ok(self
            .aggregate(
                vec![
                    doc! { "$match": {
                        "metadata.booked.milestone_index": { "$lte": ledger_index },
                        "$or": [
                            { "metadata.spent_metadata.spent": null },
                            { "metadata.spent_metadata.spent.milestone_index": { "$gt": ledger_index } },
                        ],
                    } },
                    doc! { "$group" : {
                        "_id": null,
                        "total_key_bytes": { "$sum": { "$toDecimal": "$details.rent_structure.num_key_bytes" } },
                        "total_data_bytes": { "$sum": { "$toDecimal": "$details.rent_structure.num_data_bytes" } },
                        "total_storage_deposit_value": { "$sum": { "$toDecimal": { "$ifNull": [ "$output.storage_deposit_return_unlock_condition.amount", 0 ] } } }
                    } },
                    doc! { "$project": {
                        "total_storage_deposit_value": { "$toString": "$total_storage_deposit_value" },
                        "total_key_bytes": { "$toString": "$total_key_bytes" },
                        "total_data_bytes": { "$toString": "$total_data_bytes" },
                    } },
                ],
                None,
            )
            .await?
            .try_next()
            .await?
            .unwrap_or_default())
    }
}

impl OutputCollection {
    /// Create aggregate statistics of all addresses.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_address_activity_analytics(
        &self,
        milestone_index: MilestoneIndex,
    ) -> Result<AddressActivityAnalytics, Error> {
        #[derive(Default, Deserialize)]
        struct Res {
            address_count: u64,
        }

        let (total, receiving, sending) = tokio::try_join!(
            async {
                Result::<Res, Error>::Ok(
                    self.aggregate(
                        vec![
                            doc! { "$match": {
                                "details.address": { "$exists": true },
                                "$or": [
                                    { "metadata.booked.milestone_index": milestone_index },
                                    { "metadata.spent_metadata.spent.milestone_index": milestone_index },
                                ],
                            } },
                            doc! { "$group" : { "_id": "$details.address" } },
                            doc! { "$count": "address_count" },
                        ],
                        None,
                    )
                    .await?
                    .try_next()
                    .await?
                    .unwrap_or_default(),
                )
            },
            async {
                Result::<Res, Error>::Ok(
                    self.aggregate(
                        vec![
                            doc! { "$match": {
                                "details.address": { "$exists": true },
                                "metadata.booked.milestone_index": milestone_index
                            } },
                            doc! { "$group" : { "_id": "$details.address" }},
                            doc! { "$count": "address_count" },
                        ],
                        None,
                    )
                    .await?
                    .try_next()
                    .await?
                    .unwrap_or_default(),
                )
            },
            async {
                Result::<Res, Error>::Ok(
                    self.aggregate(
                        vec![
                            doc! { "$match": {
                                "details.address": { "$exists": true },
                                "metadata.spent_metadata.spent.milestone_index": milestone_index
                            } },
                            doc! { "$group" : { "_id": "$details.address" }},
                            doc! { "$count": "address_count" },
                        ],
                        None,
                    )
                    .await?
                    .try_next()
                    .await?
                    .unwrap_or_default(),
                )
            }
        )?;
        Ok(AddressActivityAnalytics {
            total_count: total.address_count,
            receiving_count: receiving.address_count,
            sending_count: sending.address_count,
        })
    }

    /// Get ledger address analytics.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_address_analytics(&self, ledger_index: MilestoneIndex) -> Result<AddressAnalytics, Error> {
        Ok(self
            .aggregate(
                vec![
                    doc! { "$match": {
                        "details.address": { "$exists": true },
                        "metadata.booked.milestone_index": { "$lte": ledger_index },
                        "$or": [
                            { "metadata.spent_metadata.spent": null },
                            { "metadata.spent_metadata.spent.milestone_index": { "$gt": ledger_index } },
                        ],
                    } },
                    doc! { "$group" : {
                        "_id": "$details.address",
                    }},
                    doc! { "$group" : {
                        "_id": null,
                        "address_with_balance_count": { "$sum": 1 }
                    }},
                    doc! { "$project": {
                        "address_with_balance_count": "$address_with_balance_count"
                    } },
                ],
                None,
            )
            .await?
            .try_next()
            .await?
            .unwrap_or_default())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RichestAddresses {
    pub top: Vec<AddressStat>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct AddressStat {
    pub address: Address,
    pub balance: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenDistribution {
    pub distribution: Vec<DistributionStat>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// Statistics for a particular logarithmic range of balances
pub struct DistributionStat {
    /// The logarithmic index the balances are contained between: \[10^index..10^(index+1)\]
    pub index: u32,
    /// The number of unique addresses in this range
    pub address_count: u64,
    /// The total balance of the addresses in this range
    pub total_balance: String,
}

impl OutputCollection {
    /// Create richest address statistics.
    pub async fn get_richest_addresses(
        &self,
        ledger_index: MilestoneIndex,
        top: usize,
    ) -> Result<RichestAddresses, Error> {
        let top = self
            .aggregate(
                vec![
                    doc! { "$match": {
                        "details.address": { "$exists": true },
                        "metadata.booked.milestone_index": { "$lte": ledger_index },
                        "$or": [
                            { "metadata.spent_metadata.spent": null },
                            { "metadata.spent_metadata.spent.milestone_index": { "$gt": ledger_index } },
                        ]
                    } },
                    doc! { "$group" : {
                        "_id": "$details.address",
                        "balance": { "$sum": { "$toDecimal": "$output.amount" } },
                    } },
                    doc! { "$sort": { "balance": -1 } },
                    doc! { "$limit": top as i64 },
                    doc! { "$project": {
                        "_id": 0,
                        "address": "$_id",
                        "balance": { "$toString": "$balance" },
                    } },
                ],
                None,
            )
            .await?
            .try_collect()
            .await?;
        Ok(RichestAddresses { top })
    }

    /// Create token distribution statistics.
    pub async fn get_token_distribution(&self, ledger_index: MilestoneIndex) -> Result<TokenDistribution, Error> {
        let distribution = self
            .aggregate(
                vec![
                    doc! { "$match": {
                        "details.address": { "$exists": true },
                        "metadata.booked.milestone_index": { "$lte": ledger_index },
                        "$or": [
                            { "metadata.spent_metadata.spent": null },
                            { "metadata.spent_metadata.spent.milestone_index": { "$gt": ledger_index } },
                        ]
                    } },
                    doc! { "$group" : {
                        "_id": "$details.address",
                        "balance": { "$sum": { "$toDecimal": "$output.amount" } },
                    } },
                    doc! { "$set": { "index": { "$toInt": { "$log10": "$balance" } } } },
                    doc! { "$group" : {
                        "_id": "$index",
                        "address_count": { "$sum": 1 },
                        "total_balance": { "$sum": "$balance" },
                    } },
                    doc! { "$sort": { "_id": 1 } },
                    doc! { "$project": {
                        "_id": 0,
                        "index": "$_id",
                        "address_count": 1,
                        "total_balance": { "$toString": "$total_balance" },
                    } },
                ],
                None,
            )
            .await?
            .try_collect()
            .await?;
        Ok(TokenDistribution { distribution })
    }
}

impl OutputCollection {
    /// Gets the number of claimed tokens.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_claimed_token_analytics(
        &self,
        ledger_index: MilestoneIndex,
    ) -> Result<ClaimedTokensAnalytics, Error> {
        Ok(self
            .aggregate(
                vec![
                    doc! { "$match": {
                        "metadata.booked.milestone_index": { "$eq": 0 },
                        "metadata.spent_metadata.spent.milestone_index": { "$lte": ledger_index },
                    } },
                    doc! { "$group": {
                        "_id": null,
                        "claimed_count": { "$sum": 1 },
                        "claimed_value": { "$sum": { "$toDecimal": "$output.amount" } },
                    } },
                    doc! { "$project": {
                        "claimed_count": 1,
                        "claimed_value": { "$toString": "$claimed_value" },
                    } },
                ],
                None,
            )
            .await?
            .try_next()
            .await?
            .unwrap_or_default())
    }
}

impl OutputCollection {
    /// Gets analytics about unlock conditions.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_unlock_condition_analytics(
        &self,
        ledger_index: MilestoneIndex,
    ) -> Result<UnlockConditionAnalytics, Error> {
        #[derive(Default, Deserialize)]
        struct Res {
            count: u64,
            value: d128,
        }

        let query = |kind: &'static str| async move {
            Result::<Res, Error>::Ok(
                self.aggregate(
                    vec![
                        doc! { "$match": {
                            format!("output.{kind}"): { "$exists": true },
                            "metadata.booked.milestone_index": { "$lte": ledger_index },
                            "$or": [
                                { "metadata.spent_metadata.spent": null },
                                { "metadata.spent_metadata.spent.milestone_index": { "$gt": ledger_index } },
                            ]
                        } },
                        doc! { "$group": {
                            "_id": null,
                            "count": { "$sum": 1 },
                            "value": { "$sum": { "$toDecimal": "$output.amount" } },
                        } },
                        doc! { "$project": {
                            "count": 1,
                            "value": { "$toString": "$value" },
                        } },
                    ],
                    None,
                )
                .await?
                .try_next()
                .await?
                .unwrap_or_default(),
            )
        };

        let (timelock, expiration, sdruc) = tokio::try_join!(
            query("timelock_unlock_condition"),
            query("expiration_unlock_condition"),
            query("storage_deposit_return_unlock_condition"),
        )?;

        Ok(UnlockConditionAnalytics {
            timelock_count: timelock.count,
            timelock_value: timelock.value,
            expiration_count: expiration.count,
            expiration_value: expiration.value,
            storage_deposit_return_count: sdruc.count,
            storage_deposit_return_value: sdruc.value,
        })
    }
}
