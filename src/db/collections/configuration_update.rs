// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use mongodb::{bson::doc, error::Error, options::FindOneOptions};
use serde::{Deserialize, Serialize};

use crate::{
    db::{
        mongodb::{MongoDbCollection, MongoDbCollectionExt},
        MongoDb,
    },
    types::{node::NodeConfiguration, tangle::MilestoneIndex},
};

/// The corresponding MongoDb document representation to store [`NodeConfiguration`]s.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfigurationUpdateDocument {
    #[serde(rename = "_id")]
    pub ledger_index: MilestoneIndex,
    #[serde(flatten)]
    pub config: NodeConfiguration,
}

/// A collection to store [`NodeConfiguration`]s.
pub struct ConfigurationUpdateCollection {
    collection: mongodb::Collection<ConfigurationUpdateDocument>,
}

impl MongoDbCollection for ConfigurationUpdateCollection {
    const NAME: &'static str = "stardust_configuration_updates";
    type Document = ConfigurationUpdateDocument;

    fn instantiate(_db: &MongoDb, collection: mongodb::Collection<Self::Document>) -> Self {
        Self { collection }
    }

    fn collection(&self) -> &mongodb::Collection<Self::Document> {
        &self.collection
    }
}

impl ConfigurationUpdateCollection {
    /// Gets the latest node configuration.
    pub async fn get_latest_node_configuration(&self) -> Result<Option<ConfigurationUpdateDocument>, Error> {
        self.find_one(doc! {}, FindOneOptions::builder().sort(doc! { "_id": -1 }).build())
            .await
    }

    /// Gets the node configuration that was valid for the given ledger index.
    pub async fn get_node_configuration_for_ledger_index(
        &self,
        ledger_index: MilestoneIndex,
    ) -> Result<Option<ConfigurationUpdateDocument>, Error> {
        self.find_one(
            doc! { "_id": { "$lte": ledger_index } },
            FindOneOptions::builder().sort(doc! { "_id": -1 }).build(),
        )
        .await
    }

    /// Inserts or updates a node configuration for a given ledger index.
    pub async fn upsert_node_configuration(
        &self,
        ledger_index: MilestoneIndex,
        config: NodeConfiguration,
    ) -> Result<(), Error> {
        if let Some(latest_config) = self.get_node_configuration_for_ledger_index(ledger_index).await? {
            if latest_config.ledger_index == ledger_index {
                if latest_config.config != config {
                    self.replace_one(doc! {}, ConfigurationUpdateDocument { ledger_index, config }, None)
                        .await?;
                }
            } else {
                self.insert_one(ConfigurationUpdateDocument { ledger_index, config }, None)
                    .await?;
            }
        } else {
            self.insert_one(ConfigurationUpdateDocument { ledger_index, config }, None)
                .await?;
        }

        Ok(())
    }

    /// Add the node configuration to this collection if it is newer and different.
    pub async fn update_latest_node_configuration(
        &self,
        ledger_index: MilestoneIndex,
        config: NodeConfiguration,
    ) -> Result<(), Error> {
        if let Some(latest_config) = self.get_latest_node_configuration().await? {
            if latest_config.ledger_index >= ledger_index {
                return Ok(());
            } else if latest_config.config != config {
                self.insert_one(ConfigurationUpdateDocument { ledger_index, config }, None)
                    .await?;
            }
        } else {
            self.insert_one(ConfigurationUpdateDocument { ledger_index, config }, None)
                .await?;
        }
        Ok(())
    }
}
