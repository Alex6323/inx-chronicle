// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use async_trait::async_trait;
use bee_metrics::{encoding::SendSyncEncodeMetric, metrics::process::ProcessMetrics, serve_metrics, Registry};
use chronicle::runtime::{Actor, ActorContext, HandleEvent};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::oneshot,
    task::JoinHandle,
    time::{sleep, Duration},
};

pub struct MetricsWorker {
    config: MetricsConfig,
}

impl MetricsWorker {
    pub fn new(config: MetricsConfig) -> Self {
        Self { config }
    }
}

pub struct MetricsState {
    registry: Arc<Registry>,
    server_handle: (JoinHandle<()>, Option<oneshot::Sender<()>>),
    process_metrics_handle: JoinHandle<()>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsConfig {
    address: IpAddr,
    port: u16,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            address: [0, 0, 0, 0].into(),
            port: 9100,
        }
    }
}

#[async_trait]
impl Actor for MetricsWorker {
    type State = MetricsState;

    type Error = Infallible;

    async fn init(&mut self, cx: &mut ActorContext<Self>) -> Result<Self::State, Self::Error> {
        let registry = Arc::new(Registry::default());

        let addr = SocketAddr::new(self.config.address, self.config.port);

        let metrics = ProcessMetrics::new(std::process::id());
        let (mem_metric, cpu_metric) = metrics.metrics();

        registry.register("memory_usage", "Memory usage", mem_metric);
        registry.register("cpu_usage", "CPU usage", cpu_metric);

        let (send, recv) = oneshot::channel();
        let metrics_handle = cx.handle().clone();

        let server_fut = {
            let registry = registry.clone();
            tokio::spawn(async move {
                let fut = serve_metrics(addr, registry);

                // FIXME: use `with_graceful_shutdown` when `bee-metrics` exposes the complete
                // future type.
                let res = tokio::select! {
                    res = fut => { res }
                    _ = recv => { Ok(()) }
                };

                // Stop the actor if the server stops.
                metrics_handle.shutdown();

                res.unwrap()
            })
        };

        let process_metrics_handle = {
            tokio::spawn(async move {
                const MAX_RETRIES: u8 = 5;
                let mut retries = MAX_RETRIES;

                while retries > 0 {
                    sleep(Duration::from_secs(1)).await;

                    match metrics.update().await {
                        Ok(_) => {
                            retries = MAX_RETRIES;
                        }
                        Err(e) => {
                            log::warn!("Cannot update process metrics: {e}");
                            retries -= 1;
                        }
                    }
                }

                log::warn!("Could not update process metrics after {MAX_RETRIES} retries");
            })
        };

        Ok(MetricsState {
            registry,
            server_handle: (server_fut, Some(send)),
            process_metrics_handle,
        })
    }

    async fn shutdown(&mut self, cx: &mut ActorContext<Self>, state: &mut Self::State) -> Result<(), Self::Error> {
        log::debug!("{} shutting down ({})", self.name(), cx.id());

        state.process_metrics_handle.abort();

        state.server_handle.1.take().map(|send| send.send(()));

        Ok(())
    }
}

pub struct RegisterMetric<M: 'static + SendSyncEncodeMetric> {
    pub name: String,
    pub help: String,
    pub metric: M,
}

#[async_trait]
impl<M: 'static + SendSyncEncodeMetric> HandleEvent<RegisterMetric<M>> for MetricsWorker {
    async fn handle_event(
        &mut self,
        _cx: &mut ActorContext<Self>,
        event: RegisterMetric<M>,
        state: &mut Self::State,
    ) -> Result<(), Self::Error> {
        state.registry.register(event.name, event.help, event.metric);

        Ok(())
    }
}