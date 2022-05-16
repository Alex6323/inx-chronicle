// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

mod config;
pub mod solidifier;
#[cfg(all(feature = "stardust", feature = "inx"))]
pub(crate) mod stardust_inx;

use std::collections::HashMap;

use async_trait::async_trait;
use chronicle::{
    db::{bson::DocError, MongoDb},
    runtime::{Actor, ActorContext, ActorError, Addr, ConfigureActor, HandleEvent, Report, RuntimeError},
};
pub use config::CollectorConfig;
use mongodb::bson::document::ValueAccessError;
use solidifier::Solidifier;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CollectorError {
    #[error(transparent)]
    Doc(#[from] DocError),
    #[error(transparent)]
    MongoDb(#[from] mongodb::error::Error),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    ValueAccess(#[from] ValueAccessError),
}

#[derive(Debug)]
pub struct Collector {
    db: MongoDb,
    config: CollectorConfig,
}

impl Collector {
    pub fn new(db: MongoDb, config: CollectorConfig) -> Self {
        Self { db, config }
    }
}

pub struct CollectorState {
    solidifiers: HashMap<usize, Addr<Solidifier>>,
}

#[async_trait]
impl Actor for Collector {
    type State = CollectorState;
    type Error = CollectorError;

    async fn init(&mut self, cx: &mut ActorContext<Self>) -> Result<Self::State, Self::Error> {
        let mut solidifiers = HashMap::new();
        for i in 0..self.config.solidifier_count {
            solidifiers.insert(
                i,
                cx.spawn_child(Solidifier::new(i, self.db.clone()).with_registration(false))
                    .await,
            );
        }
        #[cfg(all(feature = "stardust", feature = "inx"))]
        cx.spawn_child(stardust_inx::InxWorker::new(self.db.clone(), self.config.inx.clone()))
            .await;
        Ok(CollectorState { solidifiers })
    }
}

#[async_trait]
impl HandleEvent<Report<Solidifier>> for Collector {
    async fn handle_event(
        &mut self,
        cx: &mut ActorContext<Self>,
        event: Report<Solidifier>,
        state: &mut Self::State,
    ) -> Result<(), Self::Error> {
        match event {
            Report::Success(_) => {
                cx.shutdown();
            }
            Report::Error(report) => match &report.error {
                ActorError::Result(e) => match e {
                    #[cfg(all(feature = "stardust", feature = "inx"))]
                    solidifier::SolidifierError::MissingStardustInxRequester => {
                        state
                            .solidifiers
                            .insert(report.actor.id, cx.spawn_child(report.actor).await);
                    }
                    // TODO: Maybe map Solidifier errors to Collector errors and return them?
                    _ => {
                        cx.shutdown();
                    }
                },
                ActorError::Aborted | ActorError::Panic => {
                    cx.shutdown();
                }
            },
        }
        Ok(())
    }
}
