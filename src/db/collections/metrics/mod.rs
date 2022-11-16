// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

/// Schema implementation for InfluxDb.
#[cfg(feature = "influxdb")]
pub mod influx;

use chrono::{DateTime, Utc};
use influxdb::InfluxDbWriteable;
use mongodb::bson::doc;
use serde::{Deserialize, Serialize};

use crate::types::tangle::MilestoneIndex;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "influxdb", derive(InfluxDbWriteable))]
#[allow(missing_docs)]
pub struct SyncMetrics {
    pub time: DateTime<Utc>,
    pub milestone_index: MilestoneIndex,
    pub milestone_time: u64,
    #[cfg(feature = "analytics")]
    pub analytics_time: u64,
    #[cfg_attr(feature = "influxdb", influxdb(tag))]
    pub chronicle_version: String,
    #[cfg_attr(feature = "influxdb", influxdb(tag))]
    pub network_name: String,
}
