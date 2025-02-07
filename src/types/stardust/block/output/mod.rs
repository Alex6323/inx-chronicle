// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

//! Module containing the [`Output`] types.

mod feature;
mod native_token;
mod unlock_condition;

pub mod alias;
pub mod basic;
pub mod foundry;
pub mod nft;
pub mod treasury;

use std::{borrow::Borrow, str::FromStr};

use iota_types::block::output as iota;
use mongodb::bson::{doc, Bson};
use packable::PackableExt;
use serde::{Deserialize, Serialize};

pub use self::{
    alias::{AliasId, AliasOutput},
    basic::BasicOutput,
    feature::Feature,
    foundry::{FoundryId, FoundryOutput},
    native_token::{NativeToken, NativeTokenAmount, TokenScheme},
    nft::{NftId, NftOutput},
    treasury::TreasuryOutput,
    unlock_condition::{
        AddressUnlockCondition, ExpirationUnlockCondition, GovernorAddressUnlockCondition,
        ImmutableAliasAddressUnlockCondition, StateControllerAddressUnlockCondition,
        StorageDepositReturnUnlockCondition, TimelockUnlockCondition,
    },
};
use super::Address;
use crate::types::{
    context::{TryFromWithContext, TryIntoWithContext},
    stardust::block::payload::transaction::TransactionId,
    tangle::ProtocolParameters,
};

/// The amount of tokens associated with an output.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, derive_more::From)]
pub struct OutputAmount(#[serde(with = "crate::types::util::stringify")] pub u64);

/// The index of an output within a transaction.
pub type OutputIndex = u16;

/// An id which uniquely identifies an output. It is computed from the corresponding [`TransactionId`], as well as the
/// [`OutputIndex`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct OutputId {
    /// The transaction id part of the [`OutputId`].
    pub transaction_id: TransactionId,
    /// The output index part of the [`OutputId`].
    pub index: OutputIndex,
}

impl OutputId {
    /// Converts the [`OutputId`] to its `0x`-prefixed hex representation.
    pub fn to_hex(&self) -> String {
        prefix_hex::encode([self.transaction_id.0.as_ref(), &self.index.to_le_bytes()].concat())
    }
}

impl From<iota::OutputId> for OutputId {
    fn from(value: iota::OutputId) -> Self {
        Self {
            transaction_id: (*value.transaction_id()).into(),
            index: value.index(),
        }
    }
}

impl TryFrom<OutputId> for iota::OutputId {
    type Error = iota_types::block::Error;

    fn try_from(value: OutputId) -> Result<Self, Self::Error> {
        iota::OutputId::new(value.transaction_id.into(), value.index)
    }
}

impl FromStr for OutputId {
    type Err = iota_types::block::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(iota::OutputId::from_str(s)?.into())
    }
}

impl From<OutputId> for Bson {
    fn from(val: OutputId) -> Self {
        // Unwrap: Cannot fail as type is well defined
        mongodb::bson::to_bson(&val).unwrap()
    }
}

/// Represents the different output types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Output {
    /// The [`TreasuryOutput`] variant. This is a leftover from the Chrysalis update and might be removed in the
    /// future.
    Treasury(TreasuryOutput),
    /// The [`BasicOutput`] variant.
    Basic(BasicOutput),
    /// The [`AliasOutput`] variant.
    Alias(AliasOutput),
    /// The [`FoundryOutput`] variant.
    Foundry(FoundryOutput),
    /// The [`NftOutput`] variant.
    Nft(NftOutput),
}

impl Output {
    /// Returns the [`Address`] that is in control of the output.
    pub fn owning_address(&self) -> Option<&Address> {
        Some(match self {
            Self::Treasury(_) => return None,
            Self::Basic(BasicOutput {
                address_unlock_condition,
                ..
            }) => &address_unlock_condition.address,
            Self::Alias(AliasOutput {
                state_controller_address_unlock_condition,
                ..
            }) => &state_controller_address_unlock_condition.address,
            Self::Foundry(FoundryOutput {
                immutable_alias_address_unlock_condition,
                ..
            }) => &immutable_alias_address_unlock_condition.address,
            Self::Nft(NftOutput {
                address_unlock_condition,
                ..
            }) => &address_unlock_condition.address,
        })
    }

    /// Returns the amount associated with an output.
    pub fn amount(&self) -> OutputAmount {
        match self {
            Self::Treasury(TreasuryOutput { amount, .. }) => *amount,
            Self::Basic(BasicOutput { amount, .. }) => *amount,
            Self::Alias(AliasOutput { amount, .. }) => *amount,
            Self::Nft(NftOutput { amount, .. }) => *amount,
            Self::Foundry(FoundryOutput { amount, .. }) => *amount,
        }
    }

    /// Checks if an output is trivially unlockable by only providing a signature.
    pub fn is_trivial_unlock(&self) -> bool {
        match self {
            Self::Treasury(_) => false,
            Self::Basic(BasicOutput {
                storage_deposit_return_unlock_condition,
                timelock_unlock_condition,
                expiration_unlock_condition,
                ..
            }) => {
                storage_deposit_return_unlock_condition.is_none()
                    && timelock_unlock_condition.is_none()
                    && expiration_unlock_condition.is_none()
            }
            Self::Alias(_) => true,
            Self::Nft(NftOutput {
                storage_deposit_return_unlock_condition,
                timelock_unlock_condition,
                expiration_unlock_condition,
                ..
            }) => {
                storage_deposit_return_unlock_condition.is_none()
                    && timelock_unlock_condition.is_none()
                    && expiration_unlock_condition.is_none()
            }
            Self::Foundry(_) => true,
        }
    }

    /// Converts the [`Output`] into its raw byte representation.
    pub fn raw(self, ctx: ProtocolParameters) -> Result<Vec<u8>, iota_types::block::Error> {
        let bee_output = iota_types::block::output::Output::try_from_with_context(&ctx.try_into()?, self)?;
        Ok(bee_output.pack_to_vec())
    }

    /// Get the output kind as a string.
    pub fn kind(&self) -> &str {
        match self {
            Output::Treasury(_) => TreasuryOutput::KIND,
            Output::Basic(_) => BasicOutput::KIND,
            Output::Alias(_) => AliasOutput::KIND,
            Output::Foundry(_) => FoundryOutput::KIND,
            Output::Nft(_) => NftOutput::KIND,
        }
    }
}

impl<T: Borrow<iota::Output>> From<T> for Output {
    fn from(value: T) -> Self {
        match value.borrow() {
            iota::Output::Treasury(o) => Self::Treasury(o.into()),
            iota::Output::Basic(o) => Self::Basic(o.into()),
            iota::Output::Alias(o) => Self::Alias(o.into()),
            iota::Output::Foundry(o) => Self::Foundry(o.into()),
            iota::Output::Nft(o) => Self::Nft(o.into()),
        }
    }
}

impl TryFromWithContext<Output> for iota::Output {
    type Error = iota_types::block::Error;

    fn try_from_with_context(
        ctx: &iota_types::block::protocol::ProtocolParameters,
        value: Output,
    ) -> Result<Self, Self::Error> {
        Ok(match value {
            Output::Treasury(o) => iota::Output::Treasury(o.try_into_with_context(ctx)?),
            Output::Basic(o) => iota::Output::Basic(o.try_into_with_context(ctx)?),
            Output::Alias(o) => iota::Output::Alias(o.try_into_with_context(ctx)?),
            Output::Foundry(o) => iota::Output::Foundry(o.try_into_with_context(ctx)?),
            Output::Nft(o) => iota::Output::Nft(o.try_into_with_context(ctx)?),
        })
    }
}

impl From<Output> for iota::dto::OutputDto {
    fn from(value: Output) -> Self {
        match value {
            Output::Treasury(o) => Self::Treasury(o.into()),
            Output::Basic(o) => Self::Basic(o.into()),
            Output::Alias(o) => Self::Alias(o.into()),
            Output::Foundry(o) => Self::Foundry(o.into()),
            Output::Nft(o) => Self::Nft(o.into()),
        }
    }
}

#[cfg(feature = "rand")]
mod rand {
    use iota_types::block::rand::{number::rand_number_range, output::rand_output_id};

    use super::*;

    impl OutputAmount {
        /// Generates a random [`OutputAmount`].
        pub fn rand(ctx: &iota_types::block::protocol::ProtocolParameters) -> Self {
            rand_number_range(iota::Output::AMOUNT_MIN..ctx.token_supply()).into()
        }
    }

    impl OutputId {
        /// Generates a random [`OutputId`].
        pub fn rand() -> Self {
            rand_output_id().into()
        }
    }

    impl Output {
        /// Generates a random [`Output`].
        pub fn rand(ctx: &iota_types::block::protocol::ProtocolParameters) -> Self {
            match rand_number_range(0..4) {
                0 => Self::rand_basic(ctx),
                1 => Self::rand_alias(ctx),
                2 => Self::rand_foundry(ctx),
                3 => Self::rand_nft(ctx),
                4 => Self::rand_treasury(ctx),
                _ => unreachable!(),
            }
        }

        /// Generates a random basic [`Output`].
        pub fn rand_basic(ctx: &iota_types::block::protocol::ProtocolParameters) -> Self {
            Self::Basic(BasicOutput::rand(ctx))
        }

        /// Generates a random alias [`Output`].
        pub fn rand_alias(ctx: &iota_types::block::protocol::ProtocolParameters) -> Self {
            Self::Alias(AliasOutput::rand(ctx))
        }

        /// Generates a random nft [`Output`].
        pub fn rand_nft(ctx: &iota_types::block::protocol::ProtocolParameters) -> Self {
            Self::Nft(NftOutput::rand(ctx))
        }

        /// Generates a random foundry [`Output`].
        pub fn rand_foundry(ctx: &iota_types::block::protocol::ProtocolParameters) -> Self {
            Self::Foundry(FoundryOutput::rand(ctx))
        }

        /// Generates a random treasury [`Output`].
        pub fn rand_treasury(ctx: &iota_types::block::protocol::ProtocolParameters) -> Self {
            Self::Treasury(TreasuryOutput::rand(ctx))
        }
    }
}

#[cfg(all(test, feature = "rand"))]
mod test {
    use mongodb::bson::{from_bson, to_bson};

    use super::*;

    #[test]
    fn test_output_id_bson() {
        let output_id = OutputId::rand();
        let bson = to_bson(&output_id).unwrap();
        from_bson::<OutputId>(bson).unwrap();
    }

    #[test]
    fn test_basic_output_bson() {
        let ctx = iota_types::block::protocol::protocol_parameters();
        let output = Output::rand_basic(&ctx);
        iota::Output::try_from_with_context(&ctx, output.clone()).unwrap();
        let bson = to_bson(&output).unwrap();
        assert_eq!(bson.as_document().unwrap().get_str("kind").unwrap(), BasicOutput::KIND);
        assert_eq!(output, from_bson::<Output>(bson).unwrap());
    }

    #[test]
    fn test_alias_output_bson() {
        let ctx = iota_types::block::protocol::protocol_parameters();
        let output = Output::rand_alias(&ctx);
        iota::Output::try_from_with_context(&ctx, output.clone()).unwrap();
        let bson = to_bson(&output).unwrap();
        assert_eq!(bson.as_document().unwrap().get_str("kind").unwrap(), AliasOutput::KIND);
        assert_eq!(output, from_bson::<Output>(bson).unwrap());
    }

    #[test]
    fn test_nft_output_bson() {
        let ctx = iota_types::block::protocol::protocol_parameters();
        let output = Output::rand_nft(&ctx);
        iota::Output::try_from_with_context(&ctx, output.clone()).unwrap();
        let bson = to_bson(&output).unwrap();
        assert_eq!(bson.as_document().unwrap().get_str("kind").unwrap(), NftOutput::KIND);
        assert_eq!(output, from_bson::<Output>(bson).unwrap());
    }

    #[test]
    fn test_foundry_output_bson() {
        let ctx = iota_types::block::protocol::protocol_parameters();
        let output = Output::rand_foundry(&ctx);
        iota::Output::try_from_with_context(&ctx, output.clone()).unwrap();
        let bson = to_bson(&output).unwrap();
        assert_eq!(
            bson.as_document().unwrap().get_str("kind").unwrap(),
            FoundryOutput::KIND
        );
        assert_eq!(output, from_bson::<Output>(bson).unwrap());
    }

    #[test]
    fn test_treasury_output_bson() {
        let ctx = iota_types::block::protocol::protocol_parameters();
        let output = Output::rand_treasury(&ctx);
        iota::Output::try_from_with_context(&ctx, output.clone()).unwrap();
        let bson = to_bson(&output).unwrap();
        assert_eq!(
            bson.as_document().unwrap().get_str("kind").unwrap(),
            TreasuryOutput::KIND
        );
        assert_eq!(output, from_bson::<Output>(bson).unwrap());
    }
}
