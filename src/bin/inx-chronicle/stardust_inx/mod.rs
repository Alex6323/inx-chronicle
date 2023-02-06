// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

pub mod config;
mod error;

use std::time::Duration;

use chronicle::{
    db::{
        collections::{
            ApplicationStateCollection, BlockCollection, ConfigurationUpdateCollection, LedgerUpdateCollection,
            MilestoneCollection, OutputCollection, ProtocolUpdateCollection, TreasuryCollection,
        },
        MongoDb, MongoDbCollectionExt,
    },
    inx::{BlockWithMetadataMessage, Inx, InxError, LedgerUpdateMessage, MarkerMessage},
    types::{
        ledger::{BlockMetadata, LedgerInclusionState, LedgerOutput, LedgerSpent, MilestoneIndexTimestamp},
        stardust::block::{Block, BlockId, Payload},
        tangle::MilestoneIndex,
    },
};
use eyre::{bail, Result};
use futures::{StreamExt, TryStreamExt};
use tokio::{task::JoinSet, try_join};
use tracing::{debug, info, instrument, trace, trace_span, Instrument};

pub use self::{config::InxConfig, error::InxWorkerError};

/// Batch size for insert operations.
pub const INSERT_BATCH_SIZE: usize = 1000;

pub struct InxWorker {
    db: MongoDb,
    #[cfg(any(feature = "analytics", feature = "metrics"))]
    influx_db: Option<chronicle::db::influxdb::InfluxDb>,
    config: InxConfig,
}

#[instrument(skip_all, err, level = "debug")]
#[cfg(feature = "analytics")]
pub async fn gather_analytics(
    mongodb: &MongoDb,
    influxdb: &chronicle::db::influxdb::InfluxDb,
    analytics: &mut Vec<Box<dyn chronicle::db::collections::analytics::Analytic>>,
    milestone_index: MilestoneIndex,
    milestone_timestamp: chronicle::types::stardust::milestone::MilestoneTimestamp,
) -> eyre::Result<()> {
    let mut tasks = JoinSet::new();

    let len_before = analytics.len();

    for analytic in analytics.drain(..) {
        let mongodb = mongodb.clone();
        let influxdb = influxdb.clone();
        tasks.spawn(async move {
            let mut a: Box<dyn chronicle::db::collections::analytics::Analytic> = analytic;
            if let Some(measurement) = a
                .get_measurement(&mongodb, milestone_index, milestone_timestamp)
                .await?
            {
                influxdb.insert_measurement(measurement).await?;
            }
            Ok::<_, InxWorkerError>(a)
        });
    }

    while let Some(res) = tasks.join_next().await {
        analytics.push(res??);
    }

    debug_assert_eq!(
        len_before,
        analytics.len(),
        "The number of analytics should never change."
    );

    Ok(())
}

impl InxWorker {
    /// Creates an [`Inx`] client by connecting to the endpoint specified in `inx_config`.
    pub fn new(
        db: &MongoDb,
        #[cfg(any(feature = "analytics", feature = "metrics"))] influx_db: Option<&chronicle::db::influxdb::InfluxDb>,
        inx_config: &InxConfig,
    ) -> Self {
        Self {
            db: db.clone(),
            #[cfg(any(feature = "analytics", feature = "metrics"))]
            influx_db: influx_db.cloned(),
            config: inx_config.clone(),
        }
    }

    async fn connect(&self) -> Result<Inx> {
        let url = url::Url::parse(&self.config.url)?;

        if url.scheme() != "http" {
            bail!(InxWorkerError::InvalidAddress(self.config.url.clone()));
        }

        Ok(Inx::connect(self.config.url.clone()).await?)
    }

    pub async fn run(&mut self) -> Result<()> {
        let (start_index, mut inx) = self.init().await?;

        #[cfg(feature = "analytics")]
        let starting_index = self
            .db
            .collection::<ApplicationStateCollection>()
            .get_starting_index()
            .await?
            .ok_or(InxWorkerError::MissingAppState)?;

        let mut stream = inx.listen_to_ledger_updates((start_index.0..).into()).await?;

        debug!("Started listening to ledger updates via INX.");

        #[cfg(feature = "analytics")]
        let mut analytics = match self.influx_db.as_ref() {
            None => Vec::new(),
            Some(influx_db) if influx_db.config().analytics.is_empty() => {
                chronicle::db::collections::analytics::all_analytics()
            }
            Some(influx_db) => {
                tracing::info!("Computing the following analytics: {:?}", influx_db.config().analytics);

                let mut tmp: std::collections::HashSet<chronicle::db::influxdb::config::AnalyticsChoice> =
                    influx_db.config().analytics.iter().copied().collect();
                tmp.drain().map(Into::into).collect()
            }
        };

        while let Some(ledger_update) = stream.try_next().await? {
            self.handle_ledger_update(
                &mut inx,
                ledger_update,
                &mut stream,
                #[cfg(feature = "analytics")]
                starting_index.milestone_index,
                #[cfg(feature = "analytics")]
                &mut analytics,
            )
            .await?;
        }

        tracing::debug!("INX stream closed unexpectedly.");

        Ok(())
    }

    #[instrument(skip_all, err, level = "trace")]
    async fn init(&mut self) -> Result<(MilestoneIndex, Inx)> {
        info!("Connecting to INX at bind address `{}`.", &self.config.url);
        let mut inx = self.connect().await?;
        info!("Connected to INX.");

        // Request the node status so we can get the pruning index and latest confirmed milestone
        let node_status = loop {
            match inx.read_node_status().await {
                Ok(node_status) => break node_status,
                Err(InxError::MissingField(_)) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => return Err(e.into()),
            };
        };

        debug!(
            "The node has a pruning index of `{}` and a latest confirmed milestone index of `{}`.",
            node_status.tangle_pruning_index, node_status.confirmed_milestone.milestone_info.milestone_index,
        );

        // Check if there is an unfixable gap in our node data.
        let start_index = if let Some(MilestoneIndexTimestamp {
            milestone_index: latest_milestone,
            ..
        }) = self
            .db
            .collection::<MilestoneCollection>()
            .get_newest_milestone()
            .await?
        {
            if node_status.tangle_pruning_index.0 > latest_milestone.0 {
                bail!(InxWorkerError::SyncMilestoneGap {
                    start: latest_milestone + 1,
                    end: node_status.tangle_pruning_index,
                });
            } else if node_status.confirmed_milestone.milestone_info.milestone_index.0 < latest_milestone.0 {
                bail!(InxWorkerError::SyncMilestoneIndexMismatch {
                    node: node_status.confirmed_milestone.milestone_info.milestone_index,
                    db: latest_milestone,
                });
            } else {
                latest_milestone + 1
            }
        } else {
            self.config
                .sync_start_milestone
                .max(node_status.tangle_pruning_index + 1)
        };

        let protocol_parameters = inx
            .read_protocol_parameters(start_index.0.into())
            .await?
            .params
            .inner_unverified()?;

        let node_configuration = inx.read_node_configuration().await?;

        debug!(
            "Connected to network `{}` with base token `{}[{}]`.",
            protocol_parameters.network_name(),
            node_configuration.base_token.name,
            node_configuration.base_token.ticker_symbol
        );

        self.db
            .collection::<ConfigurationUpdateCollection>()
            .update_latest_node_configuration(node_status.ledger_index, node_configuration.into())
            .await?;

        if let Some(latest) = self
            .db
            .collection::<ProtocolUpdateCollection>()
            .get_latest_protocol_parameters()
            .await?
        {
            let protocol_parameters = chronicle::types::tangle::ProtocolParameters::from(protocol_parameters);
            if latest.parameters.network_name != protocol_parameters.network_name {
                bail!(InxWorkerError::NetworkChanged(
                    latest.parameters.network_name,
                    protocol_parameters.network_name,
                ));
            }
            debug!("Found matching network in the database.");
            if latest.parameters != protocol_parameters {
                debug!("Updating protocol parameters.");
                self.db
                    .collection::<ProtocolUpdateCollection>()
                    .insert_protocol_parameters(start_index, protocol_parameters)
                    .await?;
            }

            // TODO: This is for migration purposes only. Remove it in the next version release.
            if self
                .db
                .collection::<ApplicationStateCollection>()
                .get_starting_index()
                .await?
                .is_none()
            {
                let start_timestamp = inx
                    .read_milestone(start_index.0.into())
                    .await?
                    .milestone_info
                    .milestone_timestamp
                    .into();
                self.db
                    .collection::<ApplicationStateCollection>()
                    .set_starting_index(start_index.with_timestamp(start_timestamp))
                    .await?;
            }
        } else {
            // self.db.clear().await?;
            self.db
                .collection::<OutputCollection>().drop().await?;
            self.db
                .collection::<BlockCollection>().drop().await?;
            self.db
                .collection::<MilestoneCollection>().drop().await?;
            self.db
                .collection::<LedgerUpdateCollection>().drop().await?;
            self.db
                .collection::<ConfigurationUpdateCollection>().drop().await?;
            self.db
                .collection::<TreasuryCollection>().drop().await?;


            info!("Reading unspent outputs.");
            let unspent_output_stream = inx
                .read_unspent_outputs()
                .instrument(trace_span!("inx_read_unspent_outputs"))
                .await?;

            let mut starting_index = None;

            let mut count = 0;
            let mut tasks = unspent_output_stream
                .inspect_ok(|_| count += 1)
                .map(|msg| {
                    let msg = msg?;
                    let ledger_index = &msg.ledger_index;
                    if let Some(index) = starting_index.as_ref() {
                        if index != ledger_index {
                            bail!(InxWorkerError::InvalidUnspentOutputIndex {
                                found: *ledger_index,
                                expected: *index,
                            })
                        }
                    } else {
                        starting_index = Some(*ledger_index);
                    }
                    Ok(msg)
                })
                .map(|res| Ok(res?.output))
                .try_chunks(INSERT_BATCH_SIZE)
                // We only care if we had an error, so discard the other data
                .map_err(|e| e.1)
                // Convert batches to tasks
                .try_fold(JoinSet::new(), |mut tasks, batch| async {
                    let db = self.db.clone();
                    tasks.spawn(async move { insert_unspent_outputs(&db, &batch).await });
                    Result::<_>::Ok(tasks)
                })
                .await?;

            while let Some(res) = tasks.join_next().await {
                res??;
            }

            info!("Inserted {} unspent outputs.", count);

            let starting_index = starting_index.unwrap_or_default();

            // Get the timestamp for the starting index
            let milestone_timestamp = inx
                .read_milestone(starting_index.into())
                .await?
                .milestone_info
                .milestone_timestamp
                .into();

            info!(
                "Setting starting index to {} with timestamp {}",
                starting_index,
                time::OffsetDateTime::try_from(milestone_timestamp)?
                    .format(&time::format_description::well_known::Rfc3339)?
            );

            let starting_index = starting_index.with_timestamp(milestone_timestamp);

            self.db
                .collection::<ApplicationStateCollection>()
                .set_starting_index(starting_index)
                .await?;

            info!(
                "Linking database `{}` to network `{}`.",
                self.db.name(),
                protocol_parameters.network_name()
            );

            self.db
                .collection::<ProtocolUpdateCollection>()
                .insert_protocol_parameters(start_index, protocol_parameters.into())
                .await?;
        }

        Ok((start_index, inx))
    }

    #[instrument(skip_all, fields(milestone_index, created, consumed), err, level = "debug")]
    async fn handle_ledger_update(
        &mut self,
        inx: &mut Inx,
        start_marker: LedgerUpdateMessage,
        stream: &mut (impl futures::Stream<Item = Result<LedgerUpdateMessage, InxError>> + Unpin),
        #[cfg(feature = "analytics")] synced_index: MilestoneIndex,
        #[cfg(feature = "analytics")] analytics: &mut Vec<Box<dyn chronicle::db::collections::analytics::Analytic>>,
    ) -> Result<()> {
        #[cfg(feature = "metrics")]
        let start_time = std::time::Instant::now();

        let MarkerMessage {
            milestone_index,
            consumed_count,
            created_count,
        } = start_marker.begin().ok_or(InxWorkerError::InvalidMilestoneState)?;

        trace!(
            "Received begin marker of milestone {milestone_index} with {consumed_count} consumed and {created_count} created outputs."
        );

        let mut tasks = JoinSet::new();
        let mut actual_created_count = 0;
        let mut actual_consumed_count = 0;

        stream
            .by_ref()
            .take(consumed_count)
            .map(|res| Result::<_>::Ok(res?.consumed().ok_or(InxWorkerError::InvalidMilestoneState)?))
            .inspect_ok(|_| {
                actual_consumed_count += 1;
            })
            .try_chunks(INSERT_BATCH_SIZE)
            // We only care if we had an error, so discard the other data
            .map_err(|e| e.1)
            // Convert batches to tasks
            .try_fold(&mut tasks, |tasks, batch| async {
                let db = self.db.clone();
                tasks.spawn(async move { update_spent_outputs(&db, &batch).await });
                Ok(tasks)
            })
            .await?;

        stream
            .by_ref()
            .take(created_count)
            .map(|res| Result::<_>::Ok(res?.created().ok_or(InxWorkerError::InvalidMilestoneState)?))
            .inspect_ok(|_| {
                actual_created_count += 1;
            })
            .try_chunks(INSERT_BATCH_SIZE)
            // We only care if we had an error, so discard the other data
            .map_err(|e| e.1)
            // Convert batches to tasks
            .try_fold(&mut tasks, |tasks, batch| async {
                let db = self.db.clone();
                tasks.spawn(async move { insert_unspent_outputs(&db, &batch).await });
                Ok(tasks)
            })
            .await?;

        while let Some(res) = tasks.join_next().await {
            res??;
        }

        let MarkerMessage {
            milestone_index,
            consumed_count,
            created_count,
        } = stream
            .try_next()
            .await?
            .and_then(LedgerUpdateMessage::end)
            .ok_or(InxWorkerError::InvalidMilestoneState)?;
        trace!(
            "Received end of milestone {milestone_index} with {consumed_count} consumed and {created_count} created outputs."
        );
        if actual_created_count != created_count || actual_consumed_count != consumed_count {
            bail!(InxWorkerError::InvalidLedgerUpdateCount {
                received: actual_consumed_count + actual_created_count,
                expected: consumed_count + created_count,
            });
        }

        // Record the result as part of the current span.
        tracing::Span::current().record("milestone_index", milestone_index.0);
        tracing::Span::current().record("created", created_count);
        tracing::Span::current().record("consumed", consumed_count);

        self.handle_cone_stream(inx, milestone_index).await?;
        self.handle_protocol_params(inx, milestone_index).await?;
        self.handle_node_configuration(inx, milestone_index).await?;

        let milestone = inx.read_milestone(milestone_index.0.into()).await?;

        let milestone_index: MilestoneIndex = milestone.milestone_info.milestone_index;

        let milestone_timestamp = milestone.milestone_info.milestone_timestamp.into();

        let milestone_id = milestone
            .milestone_info
            .milestone_id
            .ok_or(InxWorkerError::MissingMilestoneInfo(milestone_index))?;

        let payload =
            if let iota_types::block::payload::Payload::Milestone(payload) = milestone.milestone.inner_unverified()? {
                chronicle::types::stardust::block::payload::MilestonePayload::from(payload)
            } else {
                // The raw data is guaranteed to contain a milestone payload.
                unreachable!();
            };

        #[cfg(all(feature = "analytics", feature = "metrics"))]
        let analytics_start_time = std::time::Instant::now();
        #[cfg(feature = "analytics")]
        if milestone_index >= synced_index {
            if let Some(influx_db) = &self.influx_db {
                if influx_db.config().analytics_enabled {
                    gather_analytics(&self.db, influx_db, analytics, milestone_index, milestone_timestamp).await?;
                }
            }
        }
        #[cfg(all(feature = "analytics", feature = "metrics"))]
        {
            if let Some(influx_db) = &self.influx_db {
                if influx_db.config().analytics_enabled {
                    let analytics_elapsed = analytics_start_time.elapsed();
                    influx_db
                        .metrics()
                        .insert(chronicle::db::collections::metrics::AnalyticsMetrics {
                            time: chrono::Utc::now(),
                            milestone_index,
                            analytics_time: analytics_elapsed.as_millis() as u64,
                            chronicle_version: std::env!("CARGO_PKG_VERSION").to_string(),
                        })
                        .await?;
                }
            }
        }

        #[cfg(feature = "metrics")]
        if let Some(influx_db) = &self.influx_db {
            if influx_db.config().metrics_enabled {
                let elapsed = start_time.elapsed();
                influx_db
                    .metrics()
                    .insert(chronicle::db::collections::metrics::SyncMetrics {
                        time: chrono::Utc::now(),
                        milestone_index,
                        milestone_time: elapsed.as_millis() as u64,
                        chronicle_version: std::env!("CARGO_PKG_VERSION").to_string(),
                    })
                    .await?;
            }
        }

        // This acts as a checkpoint for the syncing and has to be done last, after everything else completed.
        self.db
            .collection::<MilestoneCollection>()
            .insert_milestone(milestone_id, milestone_index, milestone_timestamp, payload)
            .await?;

        Ok(())
    }

    #[instrument(skip_all, level = "trace")]
    async fn handle_protocol_params(&self, inx: &mut Inx, milestone_index: MilestoneIndex) -> Result<()> {
        let parameters = inx
            .read_protocol_parameters(milestone_index.0.into())
            .await?
            .params
            .inner(&())?;

        self.db
            .collection::<ProtocolUpdateCollection>()
            .update_latest_protocol_parameters(milestone_index, parameters.into())
            .await?;

        Ok(())
    }

    #[instrument(skip_all, level = "trace")]
    async fn handle_node_configuration(&self, inx: &mut Inx, milestone_index: MilestoneIndex) -> Result<()> {
        let node_configuration = inx.read_node_configuration().await?;

        self.db
            .collection::<ConfigurationUpdateCollection>()
            .update_latest_node_configuration(milestone_index, node_configuration.into())
            .await?;

        Ok(())
    }

    #[instrument(skip(self, inx), err, level = "trace")]
    async fn handle_cone_stream(&mut self, inx: &mut Inx, milestone_index: MilestoneIndex) -> Result<()> {
        let cone_stream = inx.read_milestone_cone(milestone_index.0.into()).await?;

        let mut tasks = cone_stream
            .map(|res| {
                let BlockWithMetadataMessage { block, metadata } = res?;
                Result::<_>::Ok((
                    metadata.block_id,
                    block.clone().inner_unverified()?.into(),
                    block.data(),
                    BlockMetadata::from(metadata),
                ))
            })
            .try_chunks(INSERT_BATCH_SIZE)
            .map_err(|e| e.1)
            .try_fold(JoinSet::new(), |mut tasks, batch| async {
                let db = self.db.clone();
                tasks.spawn(async move {
                    let payloads = batch
                        .iter()
                        .filter_map(|(_, block, _, metadata): &(BlockId, Block, Vec<u8>, BlockMetadata)| {
                            if metadata.inclusion_state == LedgerInclusionState::Included {
                                if let Some(Payload::TreasuryTransaction(payload)) = &block.payload {
                                    return Some((
                                        metadata.referenced_by_milestone_index,
                                        payload.input_milestone_id,
                                        payload.output_amount,
                                    ));
                                }
                            }
                            None
                        })
                        .collect::<Vec<_>>();
                    if !payloads.is_empty() {
                        db.collection::<TreasuryCollection>()
                            .insert_treasury_payloads(payloads)
                            .await?;
                    }
                    db.collection::<BlockCollection>()
                        .insert_blocks_with_metadata(batch)
                        .await?;
                    Result::<_>::Ok(())
                });
                Ok(tasks)
            })
            .await?;

        while let Some(res) = tasks.join_next().await {
            res??;
        }

        Ok(())
    }
}

#[instrument(skip_all, err, fields(num = outputs.len()), level = "trace")]
async fn insert_unspent_outputs(db: &MongoDb, outputs: &[LedgerOutput]) -> Result<()> {
    let output_collection = db.collection::<OutputCollection>();
    let ledger_collection = db.collection::<LedgerUpdateCollection>();
    try_join! {
        async {
            output_collection.insert_unspent_outputs(outputs).await?;
            Result::<_>::Ok(())
        },
        async {
            ledger_collection.insert_unspent_ledger_updates(outputs).await?;
            Ok(())
        }
    }?;
    Ok(())
}

#[instrument(skip_all, err, fields(num = outputs.len()), level = "trace")]
async fn update_spent_outputs(db: &MongoDb, outputs: &[LedgerSpent]) -> Result<()> {
    let output_collection = db.collection::<OutputCollection>();
    let ledger_collection = db.collection::<LedgerUpdateCollection>();
    try_join! {
        async {
            output_collection.update_spent_outputs(outputs).await?;
            Ok(())
        },
        async {
            ledger_collection.insert_spent_ledger_updates(outputs).await?;
            Ok(())
        }
    }
    .and(Ok(()))
}
