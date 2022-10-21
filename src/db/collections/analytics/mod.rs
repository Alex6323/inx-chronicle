// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "inx")]
mod influx;

use std::collections::{HashMap, HashSet};

use decimal::d128;
use mongodb::{bson::doc, error::Error};
use serde::{Deserialize, Serialize};

use super::{BlockCollection, OutputCollection, ProtocolUpdateCollection};
use crate::{
    db::MongoDb,
    types::{
        ledger::{BlockMetadata, LedgerInclusionState, LedgerOutput, LedgerSpent},
        stardust::block::{
            output::{AliasId, BasicOutput, FoundryId, NftId, NftOutput},
            Address, Block, Output, Payload,
        },
        tangle::{MilestoneIndex, ProtocolParameters},
    },
};

/// Holds analytics about stardust data.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct Analytics {
    pub address_activity: AddressActivityAnalytics,
    pub addresses: AddressBalances,
    pub base_token: BaseTokenActivityAnalytics,
    pub ledger_outputs: LedgerOutputAnalytics,
    pub aliases: AliasDiffTracker,
    pub native_tokens: OutputDiffTracker<FoundryId>,
    pub nfts: OutputDiffTracker<NftId>,
    pub storage_deposits: LedgerSizeAnalytics,
    pub claimed_tokens: ClaimedTokensAnalytics,
    pub payload_activity: PayloadActivityAnalytics,
    pub transaction_activity: TransactionActivityAnalytics,
    pub unlock_conditions: UnlockConditionAnalytics,
    pub protocol_params: Option<ProtocolParameters>,
}

impl Default for Analytics {
    fn default() -> Self {
        Self {
            address_activity: Default::default(),
            addresses: Default::default(),
            base_token: Default::default(),
            ledger_outputs: Default::default(),
            aliases: Default::default(),
            native_tokens: Default::default(),
            nfts: Default::default(),
            storage_deposits: Default::default(),
            claimed_tokens: Default::default(),
            payload_activity: Default::default(),
            transaction_activity: Default::default(),
            unlock_conditions: Default::default(),
            protocol_params: Default::default(),
        }
    }
}

impl Analytics {
    /// Get a processor to update the analytics with new data.
    pub fn processor(self) -> AnalyticsProcessor {
        AnalyticsProcessor {
            analytics: self,
            addresses: Default::default(),
            sending_addresses: Default::default(),
            receiving_addresses: Default::default(),
            removed_balances: Default::default(),
            removed_outputs: Default::default(),
            removed_storage_deposits: Default::default(),
            removed_unlock_conditions: Default::default(),
            spent_aliases: Default::default(),
        }
    }
}

impl MongoDb {
    /// Gets all analytics for a milestone index, fetching the data from the collections.
    #[tracing::instrument(skip(self), err, level = "trace")]
    pub async fn get_all_analytics(&self, milestone_index: MilestoneIndex) -> Result<Analytics, Error> {
        let output_collection = self.collection::<OutputCollection>();
        let block_collection = self.collection::<BlockCollection>();
        let protocol_param_collection = self.collection::<ProtocolUpdateCollection>();

        Ok(Analytics {
            address_activity: output_collection
                .get_address_activity_analytics(milestone_index)
                .await?,
            addresses: output_collection.get_address_balances(milestone_index).await?,
            base_token: output_collection
                .get_base_token_activity_analytics(milestone_index)
                .await?,
            ledger_outputs: output_collection.get_ledger_output_analytics(milestone_index).await?,
            aliases: output_collection.get_alias_output_tracker(milestone_index).await?,
            native_tokens: output_collection.get_foundry_output_tracker(milestone_index).await?,
            nfts: output_collection.get_nft_output_tracker(milestone_index).await?,
            storage_deposits: output_collection.get_ledger_size_analytics(milestone_index).await?,
            claimed_tokens: output_collection.get_claimed_token_analytics(milestone_index).await?,
            payload_activity: block_collection.get_payload_activity_analytics(milestone_index).await?,
            transaction_activity: block_collection
                .get_transaction_activity_analytics(milestone_index)
                .await?,
            unlock_conditions: output_collection
                .get_unlock_condition_analytics(milestone_index)
                .await?,
            protocol_params: protocol_param_collection
                .get_protocol_parameters_for_ledger_index(milestone_index)
                .await?
                .map(|p| p.parameters),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputDiffTracker<T: std::hash::Hash + Eq> {
    pub created: HashSet<T>,
    pub transferred: HashSet<T>,
    pub destroyed: HashSet<T>,
}

impl<T: std::hash::Hash + Eq> Default for OutputDiffTracker<T> {
    fn default() -> Self {
        Self {
            created: Default::default(),
            transferred: Default::default(),
            destroyed: Default::default(),
        }
    }
}

impl<T: std::hash::Hash + Eq> From<OutputDiffTracker<T>> for FoundryActivityAnalytics {
    fn from(value: OutputDiffTracker<T>) -> Self {
        Self {
            created_count: value.created.len() as _,
            transferred_count: value.transferred.len() as _,
            destroyed_count: value.destroyed.len() as _,
        }
    }
}

impl<T: std::hash::Hash + Eq> From<OutputDiffTracker<T>> for NftActivityAnalytics {
    fn from(value: OutputDiffTracker<T>) -> Self {
        Self {
            created_count: value.created.len() as _,
            transferred_count: value.transferred.len() as _,
            destroyed_count: value.destroyed.len() as _,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AliasDiffTracker {
    pub created: HashMap<AliasId, u32>,
    pub governor_changed: HashMap<AliasId, u32>,
    pub state_changed: HashMap<AliasId, u32>,
    pub destroyed: HashMap<AliasId, u32>,
}

impl From<AliasDiffTracker> for AliasActivityAnalytics {
    fn from(value: AliasDiffTracker) -> Self {
        Self {
            created_count: value.created.len() as _,
            governor_changed_count: value.governor_changed.len() as _,
            state_changed_count: value.state_changed.len() as _,
            destroyed_count: value.destroyed.len() as _,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AddressBalances {
    pub balances: HashMap<Address, d128>,
}

impl From<AddressBalances> for AddressAnalytics {
    fn from(value: AddressBalances) -> Self {
        Self {
            address_with_balance_count: value.balances.len() as _,
        }
    }
}

/// A processor for analytics which holds some state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnalyticsProcessor {
    analytics: Analytics,
    addresses: HashSet<Address>,
    sending_addresses: HashSet<Address>,
    receiving_addresses: HashSet<Address>,
    removed_outputs: LedgerOutputAnalytics,
    removed_storage_deposits: LedgerSizeAnalytics,
    removed_unlock_conditions: UnlockConditionAnalytics,
    removed_balances: HashMap<Address, d128>,
    spent_aliases: HashMap<AliasId, u32>,
}

impl AnalyticsProcessor {
    /// Process a protocol parameter update.
    pub fn process_protocol_params(&mut self, params: Option<ProtocolParameters>) {
        self.analytics.protocol_params = params;
    }

    /// Process a batch of created outputs.
    pub fn process_created_outputs<'a, I>(&mut self, outputs: I)
    where
        I: IntoIterator<Item = &'a LedgerOutput>,
    {
        for output in outputs {
            self.process_output(output, false);
        }
    }

    /// Process a batch of consumed outputs.
    pub fn process_consumed_outputs<'a, I>(&mut self, outputs: I)
    where
        I: IntoIterator<Item = &'a LedgerSpent>,
    {
        for output in outputs {
            self.process_output(&output.output, true);
        }
    }

    fn process_output(&mut self, output: &LedgerOutput, is_spent: bool) {
        if let Some(&address) = output.output.owning_address() {
            self.addresses.insert(address);
            if is_spent {
                self.sending_addresses.insert(address);
                *self.removed_balances.entry(address).or_default() += 1.into();
            } else {
                self.receiving_addresses.insert(address);
                *self.analytics.addresses.balances.entry(address).or_default() += 1.into();
            }
        }
        if !is_spent {
            self.analytics.base_token.transferred_value += d128::from(output.output.amount().0);
        }

        let (ledger_output_analytics, storage_deposits, unlock_conditions) = if is_spent {
            match &output.output {
                Output::Foundry(foundry) => {
                    self.analytics.native_tokens.created.remove(&foundry.foundry_id);
                    self.analytics.native_tokens.transferred.remove(&foundry.foundry_id);
                    self.analytics.native_tokens.destroyed.insert(foundry.foundry_id);
                }
                Output::Nft(nft) => {
                    self.analytics.nfts.created.remove(&nft.nft_id);
                    self.analytics.nfts.transferred.remove(&nft.nft_id);
                    self.analytics.nfts.destroyed.insert(nft.nft_id);
                }
                Output::Alias(alias) => {
                    self.analytics.aliases.created.remove(&alias.alias_id);
                    self.analytics.aliases.governor_changed.remove(&alias.alias_id);
                    self.analytics.aliases.state_changed.remove(&alias.alias_id);
                    self.analytics.aliases.destroyed.remove(&alias.alias_id);
                    self.spent_aliases.insert(alias.alias_id, alias.state_index);
                }
                _ => (),
            }
            // Spent outputs that were created by the genesis are claimed.
            if output.booked.milestone_index == 0 {
                self.analytics.claimed_tokens.claimed_count += 1;
                self.analytics.claimed_tokens.claimed_value += output.output.amount().0.into();
            }
            // To workaround spent outputs being processed first, we keep track of a separate set
            // of values which will be subtracted at the end.
            (
                &mut self.removed_outputs,
                &mut self.removed_storage_deposits,
                &mut self.removed_unlock_conditions,
            )
        } else {
            match &output.output {
                Output::Foundry(foundry) => {
                    if self.analytics.native_tokens.created.remove(&foundry.foundry_id)
                        || self.analytics.native_tokens.transferred.remove(&foundry.foundry_id)
                        || self.analytics.native_tokens.destroyed.remove(&foundry.foundry_id)
                    {
                        self.analytics.native_tokens.transferred.insert(foundry.foundry_id);
                    } else {
                        self.analytics.native_tokens.created.insert(foundry.foundry_id);
                    }
                }
                Output::Nft(nft) => {
                    if self.analytics.nfts.created.remove(&nft.nft_id)
                        || self.analytics.nfts.transferred.remove(&nft.nft_id)
                        || self.analytics.nfts.destroyed.remove(&nft.nft_id)
                    {
                        self.analytics.nfts.transferred.insert(nft.nft_id);
                    } else {
                        self.analytics.nfts.created.insert(nft.nft_id);
                    }
                }
                Output::Alias(alias) => {
                    if let Some(spent_state) = self
                        .analytics
                        .aliases
                        .created
                        .remove(&alias.alias_id)
                        .or_else(|| self.analytics.aliases.governor_changed.remove(&alias.alias_id))
                        .or_else(|| self.analytics.aliases.state_changed.remove(&alias.alias_id))
                        .or_else(|| self.analytics.aliases.destroyed.remove(&alias.alias_id))
                        .or_else(|| self.spent_aliases.remove(&alias.alias_id))
                    {
                        if alias.state_index == spent_state {
                            self.analytics
                                .aliases
                                .governor_changed
                                .insert(alias.alias_id, alias.state_index);
                        } else {
                            self.analytics
                                .aliases
                                .state_changed
                                .insert(alias.alias_id, alias.state_index);
                        }
                    } else {
                        self.analytics.aliases.created.insert(alias.alias_id, alias.state_index);
                    }
                }
                _ => (),
            }
            (
                &mut self.analytics.ledger_outputs,
                &mut self.analytics.storage_deposits,
                &mut self.analytics.unlock_conditions,
            )
        };
        match &output.output {
            Output::Treasury(_) => {
                ledger_output_analytics.treasury_count += 1;
                ledger_output_analytics.treasury_value += output.output.amount().0.into();
            }
            Output::Basic(_) => {
                ledger_output_analytics.basic_count += 1;
                ledger_output_analytics.basic_value += output.output.amount().0.into();
            }
            Output::Alias(_) => {
                ledger_output_analytics.alias_count += 1;
                ledger_output_analytics.alias_value += output.output.amount().0.into();
            }
            Output::Foundry(_) => {
                ledger_output_analytics.foundry_count += 1;
                ledger_output_analytics.foundry_value += output.output.amount().0.into();
            }
            Output::Nft(_) => {
                ledger_output_analytics.nft_count += 1;
                ledger_output_analytics.nft_value += output.output.amount().0.into();
            }
        }
        storage_deposits.total_data_bytes += output.rent_structure.num_data_bytes.into();
        storage_deposits.total_key_bytes += output.rent_structure.num_key_bytes.into();
        match output.output {
            Output::Basic(BasicOutput {
                storage_deposit_return_unlock_condition: Some(uc),
                ..
            })
            | Output::Nft(NftOutput {
                storage_deposit_return_unlock_condition: Some(uc),
                ..
            }) => {
                unlock_conditions.storage_deposit_return_count += 1;
                unlock_conditions.storage_deposit_return_value += output.output.amount().0.into();
                storage_deposits.total_storage_deposit_value += uc.amount.0.into();
            }
            _ => (),
        }
        match output.output {
            Output::Basic(BasicOutput {
                timelock_unlock_condition: Some(_),
                ..
            })
            | Output::Nft(NftOutput {
                timelock_unlock_condition: Some(_),
                ..
            }) => {
                unlock_conditions.timelock_count += 1;
                unlock_conditions.timelock_value += output.output.amount().0.into();
            }
            _ => (),
        }
        match output.output {
            Output::Basic(BasicOutput {
                expiration_unlock_condition: Some(_),
                ..
            })
            | Output::Nft(NftOutput {
                expiration_unlock_condition: Some(_),
                ..
            }) => {
                unlock_conditions.expiration_count += 1;
                unlock_conditions.expiration_value += output.output.amount().0.into();
            }
            _ => (),
        }
    }

    /// Process a batch of blocks.
    pub fn process_blocks<'a, I>(&mut self, blocks: I)
    where
        I: IntoIterator<Item = (&'a Block, &'a BlockMetadata)>,
    {
        for (block, metadata) in blocks {
            match &block.payload {
                Some(payload) => match payload {
                    Payload::Transaction(_) => self.analytics.payload_activity.transaction_count += 1,
                    Payload::Milestone(_) => self.analytics.payload_activity.milestone_count += 1,
                    Payload::TreasuryTransaction(_) => self.analytics.payload_activity.treasury_transaction_count += 1,
                    Payload::TaggedData(_) => self.analytics.payload_activity.tagged_data_count += 1,
                },
                None => self.analytics.payload_activity.no_payload_count += 1,
            }
            match &metadata.inclusion_state {
                LedgerInclusionState::Conflicting => self.analytics.transaction_activity.conflicting_count += 1,
                LedgerInclusionState::Included => self.analytics.transaction_activity.confirmed_count += 1,
                LedgerInclusionState::NoTransaction => self.analytics.transaction_activity.no_transaction_count += 1,
            }
        }
    }

    /// Complete processing and return the analytics.
    pub fn finish(mut self) -> Analytics {
        self.analytics.address_activity.total_count = self.addresses.len() as _;
        self.analytics.address_activity.receiving_count = self.receiving_addresses.len() as _;
        self.analytics.address_activity.sending_count = self.sending_addresses.len() as _;
        for (address, removed) in self.removed_balances {
            if let Some(balance) = self.analytics.addresses.balances.get_mut(&address) {
                *balance -= removed;
                if *balance == 0.into() {
                    self.analytics.addresses.balances.remove(&address);
                }
            } else {
                unreachable!("the address should always be in the map, or something is wrong");
            }
        }
        self.analytics.ledger_outputs -= self.removed_outputs;
        self.analytics.unlock_conditions -= self.removed_unlock_conditions;
        self.analytics.storage_deposits.total_storage_deposit_value -=
            self.removed_storage_deposits.total_storage_deposit_value;
        self.analytics.storage_deposits.total_data_bytes -= self.removed_storage_deposits.total_data_bytes;
        self.analytics.storage_deposits.total_key_bytes -= self.removed_storage_deposits.total_key_bytes;
        self.analytics
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressActivityAnalytics {
    /// The number of addresses used in the time period.
    pub total_count: u64,
    /// The number of addresses that received tokens in the time period.
    pub receiving_count: u64,
    /// The number of addresses that sent tokens in the time period.
    pub sending_count: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct AddressAnalytics {
    pub address_with_balance_count: u64,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct UnlockConditionAnalytics {
    pub timelock_count: u64,
    pub timelock_value: d128,
    pub expiration_count: u64,
    pub expiration_value: d128,
    pub storage_deposit_return_count: u64,
    pub storage_deposit_return_value: d128,
}

impl std::ops::Sub<UnlockConditionAnalytics> for UnlockConditionAnalytics {
    type Output = UnlockConditionAnalytics;

    fn sub(self, rhs: UnlockConditionAnalytics) -> Self::Output {
        Self {
            timelock_count: self.timelock_count - rhs.timelock_count,
            timelock_value: self.timelock_value - rhs.timelock_value,
            expiration_count: self.expiration_count - rhs.expiration_count,
            expiration_value: self.expiration_value - rhs.expiration_value,
            storage_deposit_return_count: self.storage_deposit_return_count - rhs.storage_deposit_return_count,
            storage_deposit_return_value: self.storage_deposit_return_value - rhs.storage_deposit_return_value,
        }
    }
}

impl std::ops::SubAssign<UnlockConditionAnalytics> for UnlockConditionAnalytics {
    fn sub_assign(&mut self, rhs: UnlockConditionAnalytics) {
        *self = *self - rhs
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LedgerOutputAnalytics {
    pub basic_count: u64,
    pub basic_value: d128,
    pub alias_count: u64,
    pub alias_value: d128,
    pub foundry_count: u64,
    pub foundry_value: d128,
    pub nft_count: u64,
    pub nft_value: d128,
    pub treasury_count: u64,
    pub treasury_value: d128,
}

impl std::ops::Sub<LedgerOutputAnalytics> for LedgerOutputAnalytics {
    type Output = LedgerOutputAnalytics;

    fn sub(self, rhs: LedgerOutputAnalytics) -> Self::Output {
        Self {
            basic_count: self.basic_count - rhs.basic_count,
            basic_value: self.basic_value - rhs.basic_value,
            alias_count: self.alias_count - rhs.alias_count,
            alias_value: self.alias_value - rhs.alias_value,
            foundry_count: self.foundry_count - rhs.foundry_count,
            foundry_value: self.foundry_value - rhs.foundry_value,
            nft_count: self.nft_count - rhs.nft_count,
            nft_value: self.nft_value - rhs.nft_value,
            treasury_count: self.treasury_count - rhs.treasury_count,
            treasury_value: self.treasury_value - rhs.treasury_value,
        }
    }
}

impl std::ops::SubAssign<LedgerOutputAnalytics> for LedgerOutputAnalytics {
    fn sub_assign(&mut self, rhs: LedgerOutputAnalytics) {
        *self = *self - rhs
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LedgerSizeAnalytics {
    pub total_storage_deposit_value: d128,
    pub total_key_bytes: d128,
    pub total_data_bytes: d128,
}

impl LedgerSizeAnalytics {
    pub fn total_byte_cost(&self, protocol_params: &ProtocolParameters) -> d128 {
        let rent_structure = protocol_params.rent_structure;
        d128::from(rent_structure.v_byte_cost)
            * ((self.total_data_bytes * d128::from(rent_structure.v_byte_factor_data as u32))
                + (self.total_data_bytes * d128::from(rent_structure.v_byte_factor_data as u32)))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct ClaimedTokensAnalytics {
    pub claimed_count: u64,
    pub claimed_value: d128,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct AliasActivityAnalytics {
    pub created_count: u64,
    pub governor_changed_count: u64,
    pub state_changed_count: u64,
    pub destroyed_count: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct NftActivityAnalytics {
    pub created_count: u64,
    pub transferred_count: u64,
    pub destroyed_count: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct BaseTokenActivityAnalytics {
    pub transferred_value: d128,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct FoundryActivityAnalytics {
    pub created_count: u64,
    pub transferred_count: u64,
    pub destroyed_count: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadActivityAnalytics {
    /// The number of blocks referenced by a milestone that contain a payload.
    pub transaction_count: u32,
    /// The number of blocks containing a treasury transaction payload.
    pub treasury_transaction_count: u32,
    /// The number of blocks containing a milestone payload.
    pub milestone_count: u32,
    /// The number of blocks containing a tagged data payload.
    pub tagged_data_count: u32,
    /// The number of blocks referenced by a milestone that contain no payload.
    pub no_payload_count: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionActivityAnalytics {
    /// The number of blocks containing a confirmed transaction.
    pub confirmed_count: u32,
    /// The number of blocks containing a conflicting transaction.
    pub conflicting_count: u32,
    /// The number of blocks containing no transaction.
    pub no_transaction_count: u32,
}

#[cfg(all(test, feature = "rand"))]
mod test {
    use std::collections::HashSet;

    use decimal::d128;
    use rand::Rng;

    use super::{Analytics, BaseTokenActivityAnalytics};
    use crate::{
        db::collections::analytics::{
            AddressActivityAnalytics, AddressAnalytics, AliasActivityAnalytics, ClaimedTokensAnalytics,
            FoundryActivityAnalytics, LedgerOutputAnalytics, LedgerSizeAnalytics, NftActivityAnalytics,
            PayloadActivityAnalytics, TransactionActivityAnalytics, UnlockConditionAnalytics,
        },
        types::{
            ledger::{
                BlockMetadata, ConflictReason, LedgerInclusionState, LedgerOutput, LedgerSpent,
                MilestoneIndexTimestamp, RentStructureBytes, SpentMetadata,
            },
            stardust::block::{
                output::{AliasId, AliasOutput, BasicOutput, FoundryId, FoundryOutput, NftId, NftOutput, OutputId},
                payload::TransactionId,
                Block, BlockId, Output, Payload,
            },
        },
    };

    #[test]
    fn test_analytics_processor() {
        let protocol_params = iota_types::block::protocol::protocol_parameters();

        let gov_changed_alias_id = AliasId::rand();
        let state_changed_alias_id = AliasId::rand();
        let destroyed_alias_id = AliasId::rand();

        let transferred_foundry_id = FoundryId::rand();
        let burned_foundry_id = FoundryId::rand();

        let transferred_nft_id = NftId::rand();
        let burned_nft_id = NftId::rand();

        let to_spend_outputs = std::iter::repeat_with(|| LedgerOutput {
            output_id: OutputId::rand(),
            rent_structure: RentStructureBytes {
                num_key_bytes: 0,
                num_data_bytes: 100,
            },
            output: Output::rand(&protocol_params),
            block_id: BlockId::rand(),
            booked: MilestoneIndexTimestamp {
                milestone_index: rand::thread_rng().gen_range(0..2).into(),
                milestone_timestamp: 12345.into(),
            },
        })
        .take(100)
        // Governor changed
        .chain(std::iter::once_with(|| {
            let mut output = AliasOutput::rand(&protocol_params);
            output.alias_id = gov_changed_alias_id;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Alias(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 1.into(),
                    milestone_timestamp: 12345.into(),
                },
            }
        }))
        // State index changed
        .chain(std::iter::once_with(|| {
            let mut output = AliasOutput::rand(&protocol_params);
            output.alias_id = state_changed_alias_id;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Alias(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 1.into(),
                    milestone_timestamp: 12345.into(),
                },
            }
        }))
        // Destroyed
        .chain(std::iter::once_with(|| {
            let mut output = AliasOutput::rand(&protocol_params);
            output.alias_id = destroyed_alias_id;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Alias(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 1.into(),
                    milestone_timestamp: 12345.into(),
                },
            }
        }))
        // Transferred foundry
        .chain(std::iter::once_with(|| {
            let mut output = FoundryOutput::rand(&protocol_params);
            output.foundry_id = transferred_foundry_id;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Foundry(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 1.into(),
                    milestone_timestamp: 12345.into(),
                },
            }
        }))
        // Burned foundry
        .chain(std::iter::once_with(|| {
            let mut output = FoundryOutput::rand(&protocol_params);
            output.foundry_id = burned_foundry_id;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Foundry(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 1.into(),
                    milestone_timestamp: 12345.into(),
                },
            }
        }))
        // Transferred nft
        .chain(std::iter::once_with(|| {
            let mut output = NftOutput::rand(&protocol_params);
            output.nft_id = transferred_nft_id;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Nft(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 1.into(),
                    milestone_timestamp: 12345.into(),
                },
            }
        }))
        // Burned nft
        .chain(std::iter::once_with(|| {
            let mut output = NftOutput::rand(&protocol_params);
            output.nft_id = burned_nft_id;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Nft(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 1.into(),
                    milestone_timestamp: 12345.into(),
                },
            }
        }))
        .collect::<Vec<_>>();

        let unspent_outputs = std::iter::repeat_with(|| LedgerOutput {
            output_id: OutputId::rand(),
            rent_structure: RentStructureBytes {
                num_key_bytes: 0,
                num_data_bytes: 100,
            },
            output: Output::rand(&protocol_params),
            block_id: BlockId::rand(),
            booked: MilestoneIndexTimestamp {
                milestone_index: rand::thread_rng().gen_range(0..2).into(),
                milestone_timestamp: 12345.into(),
            },
        })
        .take(100)
        // Governor changed
        .chain(std::iter::once_with(|| {
            let mut output = AliasOutput::rand(&protocol_params);
            output.alias_id = gov_changed_alias_id;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Alias(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 10.into(),
                    milestone_timestamp: 123456.into(),
                },
            }
        }))
        // State index changed
        .chain(std::iter::once_with(|| {
            let mut output = AliasOutput::rand(&protocol_params);
            output.alias_id = state_changed_alias_id;
            output.state_index = 1;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Alias(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 10.into(),
                    milestone_timestamp: 123456.into(),
                },
            }
        }))
        // Transferred foundry
        .chain(std::iter::once_with(|| {
            let mut output = FoundryOutput::rand(&protocol_params);
            output.foundry_id = transferred_foundry_id;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Foundry(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 10.into(),
                    milestone_timestamp: 123456.into(),
                },
            }
        }))
        // Transferred nft
        .chain(std::iter::once_with(|| {
            let mut output = NftOutput::rand(&protocol_params);
            output.nft_id = transferred_nft_id;
            LedgerOutput {
                output_id: OutputId::rand(),
                rent_structure: RentStructureBytes {
                    num_key_bytes: 0,
                    num_data_bytes: 100,
                },
                output: Output::Nft(output),
                block_id: BlockId::rand(),
                booked: MilestoneIndexTimestamp {
                    milestone_index: 10.into(),
                    milestone_timestamp: 123456.into(),
                },
            }
        }))
        .collect::<Vec<_>>();

        let spent_outputs = to_spend_outputs
            .iter()
            .map(|output| LedgerSpent {
                output: output.clone(),
                spent_metadata: SpentMetadata {
                    transaction_id: TransactionId::rand(),
                    spent: MilestoneIndexTimestamp {
                        milestone_index: output.booked.milestone_index + rand::thread_rng().gen_range(0..2),
                        milestone_timestamp: 23456.into(),
                    },
                },
            })
            .collect::<Vec<_>>();

        let blocks = (0..100)
            .map(|i| {
                let block = Block::rand(&protocol_params);
                let parents = block.parents.clone();
                (
                    block,
                    BlockMetadata {
                        parents,
                        is_solid: true,
                        should_promote: false,
                        should_reattach: false,
                        referenced_by_milestone_index: 1.into(),
                        milestone_index: 0.into(),
                        inclusion_state: LedgerInclusionState::Included,
                        conflict_reason: ConflictReason::None,
                        white_flag_index: i as u32,
                    },
                )
            })
            .collect::<Vec<_>>();

        let mut analytics = Analytics::default().processor();

        analytics.process_created_outputs(&to_spend_outputs);
        analytics.process_consumed_outputs(&spent_outputs);
        analytics.process_created_outputs(&unspent_outputs);
        analytics.process_blocks(blocks.iter().map(|(block, metadata)| (block, metadata)));

        let analytics = analytics.clone().finish();

        assert_eq!(
            analytics.address_activity,
            AddressActivityAnalytics {
                total_count: spent_outputs
                    .iter()
                    .map(|o| &o.output.output)
                    .chain(to_spend_outputs.iter().map(|o| &o.output))
                    .chain(unspent_outputs.iter().map(|o| &o.output))
                    .filter_map(|o| o.owning_address().cloned())
                    .collect::<HashSet<_>>()
                    .len() as _,
                receiving_count: to_spend_outputs
                    .iter()
                    .chain(unspent_outputs.iter())
                    .filter_map(|o| o.output.owning_address().cloned())
                    .collect::<HashSet<_>>()
                    .len() as _,
                sending_count: spent_outputs
                    .iter()
                    .filter_map(|o| o.output.output.owning_address().cloned())
                    .collect::<HashSet<_>>()
                    .len() as _,
            }
        );
        assert_eq!(
            AddressAnalytics::from(analytics.addresses),
            AddressAnalytics {
                address_with_balance_count: unspent_outputs
                    .iter()
                    .filter_map(|o| o.output.owning_address().cloned())
                    .collect::<HashSet<_>>()
                    .len() as _
            }
        );
        assert_eq!(
            analytics.base_token,
            BaseTokenActivityAnalytics {
                transferred_value: to_spend_outputs
                    .iter()
                    .chain(unspent_outputs.iter())
                    .map(|o| d128::from(o.output.amount().0))
                    .sum(),
            }
        );
        assert_eq!(
            analytics.ledger_outputs,
            LedgerOutputAnalytics {
                basic_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(o.output, Output::Basic(_)))
                    .count() as _,
                basic_value: unspent_outputs
                    .iter()
                    .filter(|o| matches!(o.output, Output::Basic(_)))
                    .map(|o| d128::from(o.output.amount().0))
                    .sum(),
                alias_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(o.output, Output::Alias(_)))
                    .count() as _,
                alias_value: unspent_outputs
                    .iter()
                    .filter(|o| matches!(o.output, Output::Alias(_)))
                    .map(|o| d128::from(o.output.amount().0))
                    .sum(),
                foundry_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(o.output, Output::Foundry(_)))
                    .count() as _,
                foundry_value: unspent_outputs
                    .iter()
                    .filter(|o| matches!(o.output, Output::Foundry(_)))
                    .map(|o| d128::from(o.output.amount().0))
                    .sum(),
                nft_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(o.output, Output::Nft(_)))
                    .count() as _,
                nft_value: unspent_outputs
                    .iter()
                    .filter(|o| matches!(o.output, Output::Nft(_)))
                    .map(|o| d128::from(o.output.amount().0))
                    .sum(),
                treasury_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(o.output, Output::Treasury(_)))
                    .count() as _,
                treasury_value: unspent_outputs
                    .iter()
                    .filter(|o| matches!(o.output, Output::Treasury(_)))
                    .map(|o| d128::from(o.output.amount().0))
                    .sum()
            }
        );
        assert_eq!(
            AliasActivityAnalytics::from(analytics.aliases),
            AliasActivityAnalytics {
                created_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(&o.output, Output::Alias(alias)
                        if alias.alias_id != gov_changed_alias_id && alias.alias_id != state_changed_alias_id
                    ))
                    .count() as _,
                governor_changed_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(&o.output, Output::Alias(alias) if alias.alias_id == gov_changed_alias_id))
                    .count() as _,
                state_changed_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(&o.output, Output::Alias(alias) if alias.alias_id == state_changed_alias_id))
                    .count() as _,
                destroyed_count: spent_outputs
                    .iter()
                    .filter(|o| matches!(&o.output.output, Output::Alias(alias)
                        if alias.alias_id != gov_changed_alias_id && alias.alias_id != state_changed_alias_id
                    ))
                    .count() as _,
            }
        );
        assert_eq!(
            FoundryActivityAnalytics::from(analytics.native_tokens),
            FoundryActivityAnalytics {
                created_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(&o.output, Output::Foundry(foundry) if foundry.foundry_id != transferred_foundry_id))
                    .count() as _,
                transferred_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(&o.output, Output::Foundry(foundry) if foundry.foundry_id == transferred_foundry_id))
                    .count() as _,
                destroyed_count: spent_outputs
                    .iter()
                    .filter(|o| matches!(&o.output.output, Output::Foundry(foundry) if foundry.foundry_id != transferred_foundry_id))
                    .count() as _,
            }
        );
        assert_eq!(
            NftActivityAnalytics::from(analytics.nfts),
            NftActivityAnalytics {
                created_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(&o.output, Output::Nft(nft) if nft.nft_id != transferred_nft_id))
                    .count() as _,
                transferred_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(&o.output, Output::Nft(nft) if nft.nft_id == transferred_nft_id))
                    .count() as _,
                destroyed_count: spent_outputs
                    .iter()
                    .filter(|o| matches!(&o.output.output, Output::Nft(nft) if nft.nft_id != transferred_nft_id))
                    .count() as _,
            }
        );
        assert_eq!(
            analytics.storage_deposits,
            LedgerSizeAnalytics {
                total_storage_deposit_value: unspent_outputs
                    .iter()
                    .filter_map(|o| match o.output {
                        Output::Basic(BasicOutput {
                            storage_deposit_return_unlock_condition: Some(uc),
                            ..
                        })
                        | Output::Nft(NftOutput {
                            storage_deposit_return_unlock_condition: Some(uc),
                            ..
                        }) => Some(d128::from(uc.amount.0)),
                        _ => None,
                    })
                    .sum(),
                total_key_bytes: unspent_outputs
                    .iter()
                    .map(|o| d128::from(o.rent_structure.num_key_bytes))
                    .sum(),
                total_data_bytes: unspent_outputs
                    .iter()
                    .map(|o| d128::from(o.rent_structure.num_data_bytes))
                    .sum(),
            }
        );
        assert_eq!(
            analytics.claimed_tokens,
            ClaimedTokensAnalytics {
                claimed_count: spent_outputs
                    .iter()
                    .filter(|o| o.output.booked.milestone_index == 0)
                    .count() as _,
                claimed_value: spent_outputs
                    .iter()
                    .filter(|o| o.output.booked.milestone_index == 0)
                    .map(|o| d128::from(o.output.output.amount().0))
                    .sum()
            }
        );
        assert_eq!(
            analytics.unlock_conditions,
            UnlockConditionAnalytics {
                timelock_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(
                        o.output,
                        Output::Basic(BasicOutput {
                            timelock_unlock_condition: Some(_),
                            ..
                        }) | Output::Nft(NftOutput {
                            timelock_unlock_condition: Some(_),
                            ..
                        })
                    ))
                    .count() as u64,
                timelock_value: unspent_outputs
                    .iter()
                    .filter(|o| matches!(
                        o.output,
                        Output::Basic(BasicOutput {
                            timelock_unlock_condition: Some(_),
                            ..
                        }) | Output::Nft(NftOutput {
                            timelock_unlock_condition: Some(_),
                            ..
                        })
                    ))
                    .map(|o| d128::from(o.output.amount().0))
                    .sum(),
                expiration_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(
                        o.output,
                        Output::Basic(BasicOutput {
                            expiration_unlock_condition: Some(_),
                            ..
                        }) | Output::Nft(NftOutput {
                            expiration_unlock_condition: Some(_),
                            ..
                        })
                    ))
                    .count() as u64,
                expiration_value: unspent_outputs
                    .iter()
                    .filter(|o| matches!(
                        o.output,
                        Output::Basic(BasicOutput {
                            expiration_unlock_condition: Some(_),
                            ..
                        }) | Output::Nft(NftOutput {
                            expiration_unlock_condition: Some(_),
                            ..
                        })
                    ))
                    .map(|o| d128::from(o.output.amount().0))
                    .sum(),
                storage_deposit_return_count: unspent_outputs
                    .iter()
                    .filter(|o| matches!(
                        o.output,
                        Output::Basic(BasicOutput {
                            storage_deposit_return_unlock_condition: Some(_),
                            ..
                        }) | Output::Nft(NftOutput {
                            storage_deposit_return_unlock_condition: Some(_),
                            ..
                        })
                    ))
                    .count() as u64,
                storage_deposit_return_value: unspent_outputs
                    .iter()
                    .filter(|o| matches!(
                        o.output,
                        Output::Basic(BasicOutput {
                            storage_deposit_return_unlock_condition: Some(_),
                            ..
                        }) | Output::Nft(NftOutput {
                            storage_deposit_return_unlock_condition: Some(_),
                            ..
                        })
                    ))
                    .map(|o| d128::from(o.output.amount().0))
                    .sum(),
            }
        );
        assert_eq!(
            analytics.payload_activity,
            PayloadActivityAnalytics {
                transaction_count: blocks
                    .iter()
                    .filter(|(block, _)| matches!(block.payload, Some(Payload::Transaction(_))))
                    .count() as _,
                treasury_transaction_count: blocks
                    .iter()
                    .filter(|(block, _)| matches!(block.payload, Some(Payload::TreasuryTransaction(_))))
                    .count() as _,
                milestone_count: blocks
                    .iter()
                    .filter(|(block, _)| matches!(block.payload, Some(Payload::Milestone(_))))
                    .count() as _,
                tagged_data_count: blocks
                    .iter()
                    .filter(|(block, _)| matches!(block.payload, Some(Payload::TaggedData(_))))
                    .count() as _,
                no_payload_count: blocks.iter().filter(|(block, _)| matches!(block.payload, None)).count() as _,
            }
        );
        assert_eq!(
            analytics.transaction_activity,
            TransactionActivityAnalytics {
                confirmed_count: blocks
                    .iter()
                    .filter(|(_, metadata)| matches!(metadata.inclusion_state, LedgerInclusionState::Included))
                    .count() as _,
                conflicting_count: blocks
                    .iter()
                    .filter(|(_, metadata)| matches!(metadata.inclusion_state, LedgerInclusionState::Conflicting))
                    .count() as _,
                no_transaction_count: blocks
                    .iter()
                    .filter(|(_, metadata)| matches!(metadata.inclusion_state, LedgerInclusionState::NoTransaction))
                    .count() as _
            }
        );
    }
}