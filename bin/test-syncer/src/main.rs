// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use std::{collections::{VecDeque, HashSet, HashMap}, time::Instant, sync::Arc};

use chronicle::db::MongoDb;
// use self::MongoDbDummy as MongoDb;

use chronicle::{db::{MongoDbConfig, model::{status::Status, stardust::{message::{MessageRecord, MessageMetadata}, milestone::MilestoneRecord}, sync::SyncRecord}}, dto};
use inx::{client::InxClient, tonic::Channel, proto::NoParams, NodeStatus};
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Connecting MongoDb...");

    let username = "admin";
    let password = "motdepasse";
    let mongodb_connect_url = "mongodb://localhost:27017";
    let mongodb_config = MongoDbConfig::new()
        .with_connect_url(mongodb_connect_url)
        .with_username(username)
        .with_password(password);
    let mongodb = MongoDb::connect(&mongodb_config).await.unwrap();

    if let Some(db_status) = mongodb.status().await.unwrap() {
        println!("{:?}", db_status);
    } else {
        println!("No node status has been found in the database, it seems like the database is empty.");
    };

    // let inx_connect_url = "http://116.203.35.39:9029";
    let inx_connect_url = "http://localhost:9029";
    println!("Connecting node via INX at bind address `{}`.", inx_connect_url);
    let mut inx = Inx::connect(inx_connect_url.to_string()).await?;

    let node_status: NodeStatus =
        inx
        .read_node_status(NoParams {})
        .await?
        .into_inner()
        .try_into()
        .unwrap();

    if !node_status.is_healthy {
        println!("Node is unhealthy.");
        return Ok(())
    } 

    println!("Node's ledger index `{}`.", node_status.ledger_index);
    println!("Node's pruning index `{}`.", node_status.pruning_index);

    let start_index = node_status.pruning_index + 1;
    let target_index = node_status.latest_milestone.milestone_index;

    let mut syncer = Syncer::new(mongodb.clone(), inx.clone());
    println!("Syncing range [{}:{}]", start_index, target_index);
    syncer.sync(start_index, target_index).await?;

    Ok(())
}

struct Inx;

impl Inx {
    async fn connect(connect_url: String) -> Result<InxClient<Channel>, Box<dyn std::error::Error>> {
        let url = url::Url::parse(&connect_url)?;

        if url.scheme() != "http" {
            panic!("url error");
        }

        Ok(InxClient::connect(connect_url.clone())
            .await?)
    }
}

struct Syncer {
    db: MongoDb,
    inx: InxClient<Channel>,
}

impl Syncer {
    fn new(db: MongoDb, inx: InxClient<Channel>) -> Self {
        Self { db, inx }
    }

    async fn sync(&mut self, start_index: u32, target_index: u32) -> Result<(), Box<dyn std::error::Error>> {
        let mut solidifier = Solidifier::new(self.db.clone(), self.inx.clone());

        for index in start_index..target_index {
            let milestone = self.inx.read_milestone(inx::proto::MilestoneRequest {
                milestone_index: index,
                milestone_id: None,
            })
            .await.unwrap().into_inner();

            let milestone = milestone.try_into().unwrap();
            self.db.upsert_milestone_record(&milestone).await?;

            let parents = Vec::from(milestone.payload.essence.parents);
            let ms_state = MilestoneState::new(milestone.milestone_index, parents);

            println!("Solidifying {}", index);
            solidifier.solidify(ms_state).await?;
        }

        Ok(())
    }
}

struct Solidifier {
    db: MongoDb,
    inx: InxClient<Channel>,
}

impl Solidifier {

    fn new(db: MongoDb, inx: InxClient<Channel>) -> Self {
        Self { db, inx }
    }

    async fn solidify(&mut self, mut ms_state: MilestoneState) -> Result<(), Box<dyn std::error::Error>> {

        let mut num_visited = 0usize;
        let mut num_not_visited = 0usize;
        let mut num_previous_ms = 0usize;
        let mut num_update_md = 0usize;
        let mut num_upsert_msg = 0usize;

        let while_let_now = Instant::now();
        while let Some(current_message_id) = ms_state.process_queue.pop_front() {
            if ms_state.visited.contains(&current_message_id) {
                num_visited += 1;
                continue;
            }

            num_not_visited += 1;

            // let get_message_now = Instant::now();
            match self.db.get_message(&current_message_id).await.expect("db.get_message") {
                Some(msg) => {
                    // println!("db.get_message ~> Some {}s", get_message_now.elapsed().as_secs_f32());
                    if let Some(md) = msg.metadata {
                        ms_state.visited.insert(current_message_id);

                        let referenced_index = md.referenced_by_milestone_index;
                        if referenced_index != ms_state.milestone_index {
                            num_previous_ms += 1;
                            continue;
                        }

                        let parents = msg.message.parents.to_vec();
                        ms_state.process_queue.extend(parents);
                    } else if let Some(metadata) = read_metadata(&mut self.inx, current_message_id.clone()).await {
                            self.db
                                .update_message_metadata(&current_message_id, &metadata)
                                .await
                                .expect("update_message_metadata");

                            num_update_md += 1;
                            ms_state.process_queue.push_back(current_message_id);
                    }
                }
                None => {
                    // println!("db.get_message ~> None {}s", get_message_now.elapsed().as_secs_f32());
                    if let Some(message) = read_message(&mut self.inx, current_message_id.clone()).await {
                        self.db
                            .upsert_message_record(&message)
                            .await
                            .expect("upsert_message_record");

                        num_upsert_msg += 1;
                        ms_state.process_queue.push_back(current_message_id);
                    }
                }
            }
        }
        println!("while let {}s", while_let_now.elapsed().as_secs_f32());

        // If we finished all the parents, that means we have a complete milestone
        // so we should mark it synced
        self.db
            .upsert_sync_record(&SyncRecord {
                milestone_index: ms_state.milestone_index,
                logged: false,
                synced: true,
            })
            .await?;

        println!("
            num_visited: {num_visited}, 
            num_not_visited: {num_not_visited},
            num_previous_ms: {num_previous_ms},
            num_update_md: {num_update_md},
            num_upsert_msg: {num_upsert_msg}",
        );

        println!(
            "Milestone '{}' synced in {}s.",
            ms_state.milestone_index,
            ms_state.time.elapsed().as_secs_f32()
        );

        Ok(())
    }
}

#[derive(Debug)]
pub struct MilestoneState {
    pub milestone_index: u32,
    pub process_queue: VecDeque<dto::MessageId>,
    pub visited: HashSet<dto::MessageId>,
    pub time: Instant,
}

impl MilestoneState {
    pub fn new(milestone_index: u32, parents: Vec<dto::MessageId>) -> Self {
        Self {
            milestone_index,
            process_queue: parents.into(),
            visited: HashSet::new(),
            time: Instant::now(),
        }
    }
}

async fn read_message(inx: &mut InxClient<Channel>, message_id: dto::MessageId) -> Option<MessageRecord> {
    if let (Ok(message), Ok(metadata)) = (
        inx.read_message(inx::proto::MessageId {
            id: message_id.0.clone().into(),
        })
        .await,
        inx.read_message_metadata(inx::proto::MessageId {
            id: message_id.0.into(),
        })
        .await,
    ) {
        // let now = Instant::now();
        let raw = message.into_inner();
        let metadata = metadata.into_inner();
        let message = MessageRecord::try_from((raw, metadata)).unwrap();

        Some(message)
    } else {
        None
    }
}

async fn read_metadata(inx: &mut InxClient<Channel>, message_id: dto::MessageId) -> Option<MessageMetadata> {
    if let Ok(metadata) = inx
        .read_message_metadata(inx::proto::MessageId {
            id: message_id.0.into(),
        })
        .await
    {
        // let now = Instant::now();
        let metadata: inx::MessageMetadata = metadata.into_inner().try_into().unwrap();
        let metadata = metadata.into();

        Some(metadata)
    } else {
        None
    }
}

#[derive(Clone, Default)]
struct MongoDbDummy {
    messages: Arc<Mutex<HashMap<dto::MessageId, MessageRecord>>>,
    metadata: Arc<Mutex<HashMap<dto::MessageId, MessageMetadata>>>,
    milestones: Arc<Mutex<HashMap<dto::MilestoneIndex, MilestoneRecord>>>,
    sync_records: Arc<Mutex<HashMap<dto::MilestoneIndex, SyncRecord>>>,
}

impl MongoDbDummy {
    async fn connect(_config: &MongoDbConfig) -> Result<MongoDbDummy, Box<dyn std::error::Error>> {
        Ok(MongoDbDummy::default())
    }

    async fn status(&self) -> Result<Option<Status>, Box<dyn std::error::Error>> {
        Ok(None)
    }

    async fn get_message(&self, message_id: &dto::MessageId) -> Result<Option<MessageRecord>, Box<dyn std::error::Error>> {
        Ok(self.messages.lock().await.get(message_id).cloned())
    }

    async fn upsert_milestone_record(&self, milestone: &MilestoneRecord) -> Result<(), Box<dyn std::error::Error>> {
        let milestone_index = milestone.milestone_index;
        self.milestones.lock().await.insert(milestone_index, milestone.clone());
        Ok(())
    }

    async fn upsert_sync_record(&self, sync_record: &SyncRecord)  -> Result<(), Box<dyn std::error::Error>> {
        let milestone_index = sync_record.milestone_index;
        self.sync_records.lock().await.insert(milestone_index, sync_record.clone());
        Ok(())
    }

    async fn upsert_message_record(&self, message: &MessageRecord) -> Result<(), Box<dyn std::error::Error>> {
        let message_id = &message.message.id;
        self.messages.lock().await.insert(message_id.clone(), message.clone());
        Ok(())
    }
    
    async fn update_message_metadata(&self, message_id: &dto::MessageId, metadata: &MessageMetadata) -> Result<(), Box<dyn std::error::Error>> {
        self.metadata.lock().await.insert(message_id.clone(), metadata.clone());
        Ok(())
    }
}
