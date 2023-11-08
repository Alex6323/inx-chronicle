// Copyright 2023 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use iota_sdk::types::block::{payload::Payload, SignedBlock};
use packable::PackableExt;

use crate::{
    analytics::{Analytics, AnalyticsContext},
    model::block_metadata::BlockMetadata,
};

/// Slot size statistics.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct SlotSizeMeasurement {
    pub(crate) total_tagged_data_payload_bytes: usize,
    pub(crate) total_transaction_payload_bytes: usize,
    pub(crate) total_candidacy_announcement_payload_bytes: usize,
    pub(crate) total_slot_bytes: usize,
}

impl Analytics for SlotSizeMeasurement {
    type Measurement = Self;

    fn handle_block(&mut self, block: &SignedBlock, _metadata: &BlockMetadata, _ctx: &dyn AnalyticsContext) {
        let byte_len = block.packed_len();
        self.total_slot_bytes += byte_len;
        match block.block().as_basic_opt().and_then(|b| b.payload()) {
            Some(Payload::TaggedData(_)) => self.total_tagged_data_payload_bytes += byte_len,
            Some(Payload::SignedTransaction(_)) => self.total_transaction_payload_bytes += byte_len,
            Some(Payload::CandidacyAnnouncement(_)) => self.total_candidacy_announcement_payload_bytes += byte_len,
            _ => {}
        }
    }

    fn take_measurement(&mut self, _ctx: &dyn AnalyticsContext) -> Self::Measurement {
        std::mem::take(self)
    }
}
