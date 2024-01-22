// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use iota_sdk::types::api::core::response as iota;
use mongodb::bson::Bson;
use serde::{Deserialize, Serialize};

/// A block's ledger inclusion state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerInclusionState {
    /// A conflicting block, ex. a double spend
    Conflicting,
    /// A successful, included block
    Included,
    /// A block without a transaction
    NoTransaction,
}

impl From<LedgerInclusionState> for Bson {
    fn from(val: LedgerInclusionState) -> Self {
        // Unwrap: Cannot fail as type is well defined
        mongodb::bson::to_bson(&val).unwrap()
    }
}

impl From<iota::LedgerInclusionState> for LedgerInclusionState {
    fn from(value: iota::LedgerInclusionState) -> Self {
        match value {
            iota::LedgerInclusionState::Conflicting => Self::Conflicting,
            iota::LedgerInclusionState::Included => Self::Included,
            iota::LedgerInclusionState::NoTransaction => Self::NoTransaction,
        }
    }
}

impl From<LedgerInclusionState> for iota::LedgerInclusionState {
    fn from(value: LedgerInclusionState) -> Self {
        match value {
            LedgerInclusionState::Conflicting => Self::Conflicting,
            LedgerInclusionState::Included => Self::Included,
            LedgerInclusionState::NoTransaction => Self::NoTransaction,
        }
    }
}