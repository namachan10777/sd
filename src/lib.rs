use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, time::Duration};

#[derive(Serialize, Deserialize)]
pub struct RegisterRequest {
    pub tag: Tag,
    #[serde(with = "humantime_serde")]
    pub keepalive: Duration,
    pub name: Option<Name>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Success,
    WhatIsYourName,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Tag(pub String);

impl Serialize for Tag {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Tag {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Tag(s))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum Name {
    #[serde(rename = "addr")]
    Addr(SocketAddr),
    #[serde(rename = "host")]
    Host(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    Working,
    NotWorking,
}

#[async_trait::async_trait]
pub trait AsyncReporter {
    async fn report(&self, status: ServiceStatus);
    async fn change_name(&self, name: Option<Name>);
}

#[derive(Debug, thiserror::Error)]
pub enum RepoterError {
    #[error("transport {0}")]
    Transport(reqwest::Error),
}

pub mod tokio {
    use std::{
        future::Future,
        io,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    use tokio::sync::{mpsc, RwLock};
    use tracing::error;

    use crate::{AsyncReporter, Name, RegisterRequest, RepoterError, ServiceStatus, Tag};

    #[derive(Debug, thiserror::Error)]
    pub enum ReporterBuildError {
        #[error("interval larger than keepalive")]
        IntervalLargerThanKeepalive,
        #[error("timeout larger than interval")]
        TimeoutLargerThanInterval,
        #[error("create HTTP client")]
        CreateHttpClient(reqwest::Error),
    }

    pub struct Builder {
        endpoint: String,
        tag: String,
        keepalive: Duration,
        interval: Option<Duration>,
        timeout: Option<Duration>,
        reqwest_client: Option<reqwest::Client>,
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Error {
        #[error("transport {0}")]
        Transport(reqwest::Error),
    }

    #[derive(Debug)]
    enum Command {
        Status(ServiceStatus),
        Name(Option<Name>),
    }

    #[async_trait::async_trait]
    impl AsyncReporter for mpsc::Sender<Command> {
        async fn report(&self, status: ServiceStatus) {
            self.send(Command::Status(status)).await.unwrap();
        }

        async fn change_name(&self, name: Option<Name>) {
            self.send(Command::Name(name)).await.unwrap();
        }
    }

    impl Builder {
        pub fn new(endpoint: String, tag: String) -> Self {
            Self {
                endpoint,
                tag,
                keepalive: Duration::from_secs(30),
                interval: None,
                timeout: None,
                reqwest_client: None,
            }
        }

        pub fn keepalive(&mut self, keepalive: Duration) -> &mut Self {
            self.keepalive = keepalive;
            self
        }

        pub fn interval(&mut self, interval: Duration) -> &mut Self {
            self.interval = Some(interval);
            self
        }

        pub fn timeout(&mut self, timeout: Duration) -> &mut Self {
            self.timeout = Some(timeout);
            self
        }

        pub fn http_client(&mut self, client: reqwest::Client) -> &mut Self {
            self.reqwest_client = Some(client);
            self
        }

        async fn report(
            endpoint: &str,
            client: &reqwest::Client,
            tx: &mpsc::Sender<RepoterError>,
            tag: Tag,
            keepalive: Duration,
            name: Option<Name>,
            alive: bool,
        ) {
            if let Err(e) = if alive {
                client.put(endpoint)
            } else {
                client.delete(endpoint)
            }
            .json(&RegisterRequest {
                tag,
                keepalive,
                name,
            })
            .send()
            .await
            {
                if let Err(e) = tx.send(RepoterError::Transport(e)).await {
                    error!("failed to send cmd due to {}", e);
                }
            }
        }

        pub fn build(
            &mut self,
        ) -> Result<(impl Future<Output = io::Result<()>>, impl AsyncReporter), ReporterBuildError>
        {
            let (heatbeater, reporter, _) = self.build_with_notifier()?;
            Ok((heatbeater, reporter))
        }

        pub fn build_with_notifier(
            &mut self,
        ) -> Result<
            (
                impl Future<Output = io::Result<()>>,
                impl AsyncReporter,
                mpsc::Receiver<RepoterError>,
            ),
            ReporterBuildError,
        > {
            let endpoint = self.endpoint.clone();
            let keepalive = self.keepalive;
            let interval = self.interval.unwrap_or(keepalive / 2);
            let timeout = self.timeout.unwrap_or(interval / 4);
            let client = self
                .reqwest_client
                .as_ref()
                .cloned()
                .map(Ok)
                .unwrap_or_else(|| reqwest::Client::builder().timeout(timeout).build())
                .map_err(ReporterBuildError::CreateHttpClient)?;

            if interval > keepalive {
                return Err(ReporterBuildError::IntervalLargerThanKeepalive);
            }
            if timeout > interval {
                return Err(ReporterBuildError::TimeoutLargerThanInterval);
            }

            let (internal_tx, mut internal_rx) = mpsc::channel(1024);
            let (tx, rx) = mpsc::channel(1024);

            let name = Arc::new(RwLock::new(None));
            let alive = Arc::new(AtomicBool::new(false));
            let tag = Tag(self.tag.clone());

            let event_handler = {
                let name = name.clone();
                let alive = alive.clone();
                let tag = tag.clone();
                let client = client.clone();
                let endpoint = endpoint.clone();
                let tx = tx.clone();
                async move {
                    loop {
                        match internal_rx.recv().await {
                            Some(Command::Name(new_name)) => {
                                Self::report(
                                    &endpoint,
                                    &client,
                                    &tx,
                                    tag.clone(),
                                    keepalive,
                                    name.read().await.clone(),
                                    false,
                                )
                                .await;
                                {
                                    *name.write().await = new_name.clone();
                                }
                                Self::report(
                                    &endpoint,
                                    &client,
                                    &tx,
                                    tag.clone(),
                                    keepalive,
                                    new_name,
                                    true,
                                )
                                .await;
                            }
                            Some(Command::Status(ServiceStatus::NotWorking)) | None => {
                                alive.store(false, Ordering::SeqCst);
                                Self::report(
                                    &endpoint,
                                    &client,
                                    &tx,
                                    tag.clone(),
                                    keepalive,
                                    name.read().await.clone(),
                                    false,
                                )
                                .await;
                            }
                            Some(Command::Status(ServiceStatus::Working)) => {
                                alive.store(true, Ordering::SeqCst);
                                Self::report(
                                    &endpoint,
                                    &client,
                                    &tx,
                                    tag.clone(),
                                    keepalive,
                                    name.read().await.clone(),
                                    true,
                                )
                                .await;
                            }
                        }
                    }
                }
            };

            let heatbeater = async move {
                let event_handler = tokio::spawn(event_handler);
                loop {
                    let now = Instant::now();
                    if event_handler.is_finished() {
                        break;
                    }
                    Self::report(
                        &endpoint,
                        &client,
                        &tx,
                        tag.clone(),
                        keepalive,
                        name.read().await.clone(),
                        alive.load(Ordering::SeqCst),
                    )
                    .await;
                    let elapsed = now.elapsed();
                    if elapsed < interval {
                        tokio::time::sleep(interval - elapsed).await;
                    }
                }
                if let Err(e) = event_handler.await {
                    error!("event handler is dead {}", e);
                    Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        format!("event handler is dead due to {}", e),
                    ))
                } else {
                    unreachable!()
                }
            };

            Ok((heatbeater, internal_tx, rx))
        }
    }
}
