// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    time::Duration,
};

use async_trait::async_trait;
use chronicle::{
    db::MongoDb,
    runtime::{
        actor::{context::ActorContext, event::HandleEvent, Actor},
        error::RuntimeError,
    },
};
use inx::NodeStatus;
use serde::{Deserialize, Serialize};

use crate::inx::{InxRequest, InxWorker};

// solidifying a milestone must never take longer than the coordinator milestone interval
const MAX_SYNC_TIME: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum InxSyncerError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    MongoDb(#[from] mongodb::error::Error),
    #[error(transparent)]
    Bson(#[from] mongodb::bson::de::Error),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InxSyncerConfig {
    // the maximum number of simultaneously open requests for milestones
    pub(crate) max_simultaneous_requests: usize,
    // the maximum number of requests for a single milestone
    pub(crate) max_request_retries: usize,
    // the number of historic milestones the Syncer tries to sync from the ledger index at start
    pub(crate) sync_back_delta: u32,
    // the fixed milestone index of a historic milestone the Syncer tries to sync back to
    pub(crate) sync_back_index: u32, // if set != 0 in the config, will override any also configured delta
}

impl Default for InxSyncerConfig {
    fn default() -> Self {
        Self {
            max_simultaneous_requests: 10,
            max_request_retries: 3,
            sync_back_delta: 10000,
            sync_back_index: 0,
        }
    }
}

// The Syncer goes backwards in time and tries collect as many milestones as possible.
#[derive(Debug)]
pub struct InxSyncer {
    db: MongoDb,
    config: InxSyncerConfig,
    internal_state: Option<SyncerState>,
}

impl InxSyncer {
    pub fn new(db: MongoDb, config: InxSyncerConfig) -> Self {
        Self {
            db,
            config,
            internal_state: None,
        }
    }

    pub fn with_internal_state(mut self, internal_state: SyncerState) -> Self {
        self.internal_state.replace(internal_state);
        self
    }

    async fn is_synced(&self, index: u32) -> Result<bool, InxSyncerError> {
        let sync_record = self.db.get_sync_record_by_index(index).await?;
        Ok(sync_record.map_or(false, |rec| rec.synced))
    }

    fn get_start_ms_index(&self, target_index: u32, sync_state: &SyncerState) -> u32 {
        // if the user specified a concrete sync start index then ignore
        // the `sync_back_delta` configuration.
        if self.config.sync_back_index != 0 {
            self.config.sync_back_index.max(sync_state.start_ms_index)
        } else if self.config.sync_back_delta != 0 {
            target_index
                .checked_sub(self.config.sync_back_delta)
                .unwrap_or(1)
                .max(sync_state.start_ms_index)
        } else {
            sync_state.start_ms_index
        }
    }
}

struct NextMilestone {
    index: u32,
}
pub(crate) struct NewSyncedMilestone(pub(crate) u32);
pub(crate) struct NewTargetMilestone(pub(crate) u32);

#[derive(Debug, Default)]
pub struct SyncerState {
    // lower bound of the syncer range
    start_ms_index: u32,
    // upper bound of the syncer range
    target_ms_index: u32,
    // the set of currently synced milestones
    pending: HashMap<u32, usize>,
}

#[async_trait]
impl Actor for InxSyncer {
    type State = SyncerState;
    type Error = InxSyncerError;

    async fn init(&mut self, cx: &mut ActorContext<Self>) -> Result<Self::State, Self::Error> {
        // Send a `NodeStatus` request to the `InxWorker`
        cx.addr::<InxWorker>().await.send(InxRequest::NodeStatus)?;

        let internal_state = self.internal_state.take();
        Ok(internal_state.unwrap_or_default())
    }
}

// issues requests in a controlled way
#[async_trait]
impl HandleEvent<NextMilestone> for InxSyncer {
    async fn handle_event(
        &mut self,
        cx: &mut ActorContext<Self>,
        NextMilestone { index }: NextMilestone,
        syncer_state: &mut Self::State,
    ) -> Result<(), Self::Error> {
        if index > syncer_state.target_ms_index {
            log::info!("Syncer finished at target index '{}'.", syncer_state.target_ms_index);
            return Ok(());
        }
        if syncer_state.pending.len() < self.config.max_simultaneous_requests {
            if !self.is_synced(index).await? {
                log::info!("Requesting milestone '{}'.", index);
                cx.addr::<InxWorker>()
                    .await
                    .send(InxRequest::milestone(index.into(), cx.addr().await))?;
                syncer_state.pending.insert(index, self.config.max_request_retries);
            }
            cx.delay(NextMilestone { index: index + 1 }, None)?;
        } else {
            cx.delay(NextMilestone { index }, Duration::from_secs_f32(0.5))?;
        }
        Ok(())
    }
}

#[async_trait]
impl HandleEvent<NodeStatus> for InxSyncer {
    async fn handle_event(
        &mut self,
        _: &mut ActorContext<Self>,
        node_status: NodeStatus,
        syncer_state: &mut Self::State,
    ) -> Result<(), Self::Error> {
        log::trace!(
            "Syncer received node status (pruning index = '{}').",
            node_status.pruning_index
        );
        syncer_state.start_ms_index = node_status.pruning_index + 1;
        Ok(())
    }
}

// removes successfully synced milestones from `pending` or `retrying`
#[async_trait]
impl HandleEvent<NewSyncedMilestone> for InxSyncer {
    async fn handle_event(
        &mut self,
        cx: &mut ActorContext<Self>,
        NewSyncedMilestone(latest_synced_index): NewSyncedMilestone,
        sync_state: &mut Self::State,
    ) -> Result<(), Self::Error> {
        log::debug!("Syncer received new synced milestone '{}'", latest_synced_index);

        if sync_state.pending.remove(&latest_synced_index).is_some() {
        } else if sync_state.target_ms_index == 0 {
            // Set the target to the first synced milestone that was not requested by the Syncer.
            sync_state.target_ms_index = latest_synced_index;
            sync_state.start_ms_index = self.get_start_ms_index(latest_synced_index, sync_state);

            log::info!(
                "Start syncing milestone range: [{}:{}]",
                sync_state.start_ms_index,
                sync_state.target_ms_index
            );
            cx.delay(NextMilestone { index: sync_state.start_ms_index }, None)?;
        }
        Ok(())
    }
}

// allows to resume the syncer
#[async_trait]
impl HandleEvent<NewTargetMilestone> for InxSyncer {
    async fn handle_event(
        &mut self,
        cx: &mut ActorContext<Self>,
        NewTargetMilestone(new_target_ms_index): NewTargetMilestone,
        syncer_state: &mut Self::State,
    ) -> Result<(), Self::Error> {
        log::trace!("Syncer received new target milestone '{}'", new_target_ms_index);

        if new_target_ms_index > syncer_state.target_ms_index {
            let previous_target = syncer_state.target_ms_index;
            syncer_state.target_ms_index = new_target_ms_index;
            let start_index = previous_target + 1;
            log::info!(
                "Start syncing milestone range: [{}:{}]",
                start_index,
                new_target_ms_index
            );
            cx.delay(NextMilestone { index: start_index }, None)?;
        }
        Ok(())
    }
}