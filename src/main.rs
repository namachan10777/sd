use clap::Parser;
use sd::Tag;
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};
use tracing::{info, log::warn, trace};

use tokio::{sync::RwLock, time::Instant};
use warp::{hyper::StatusCode, reply::with_status, Filter};

#[derive(Clone)]
struct Server {
    inner: Arc<RwLock<HashMap<(sd::Name, sd::Tag), Instant>>>,
}

#[derive(Deserialize)]
pub struct Query {
    pub tag: Option<Tag>,
}

impl Server {
    fn new() -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

#[derive(clap::Parser)]
struct Opts {
    #[clap(short, long, required_unless_present = "example")]
    addr: Option<SocketAddr>,
    #[clap(conflicts_with = "addr", short, long)]
    example: bool,
}

#[tokio::main]
async fn main() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(fmt::layer())
        .init();

    let opts = Opts::parse();

    if opts.example {
        println!(
            "{}",
            serde_json::to_string(&sd::RegisterRequest {
                tag: Tag("hoge".to_owned()),
                name: Some(sd::Name::Addr("192.168.1.5:8080".parse().unwrap())),
                keepalive: Duration::from_secs(30),
            })
            .unwrap()
        );
        return;
    }

    let server = Server::new();
    let state = warp::any().map(move || server.clone());

    let query = state.clone().and(warp::get()).and(warp::query()).then(
        |state: Server, query: Query| async move {
            let names = state
                .inner
                .read()
                .await
                .keys()
                .filter_map(|(name, tag)| {
                    if let Some(query_tag) = &query.tag {
                        if tag == query_tag {
                            Some(name.clone())
                        } else {
                            None
                        }
                    } else {
                        Some(name.clone())
                    }
                })
                .collect::<Vec<_>>();
            warp::reply::json(&names)
        },
    );

    let register = state
        .clone()
        .and(warp::put())
        .and(warp::body::json())
        .and(warp::filters::addr::remote())
        .then(
            |state: Server, body: sd::RegisterRequest, addr: Option<SocketAddr>| async move {
                let name = match (body.name.clone(), addr) {
                    (Some(name), _) => name,
                    (None, Some(addr)) => sd::Name::Addr(addr),
                    (None, None) => {
                        warn!("sender-unknown: {}", serde_json::to_string(&body).unwrap());
                        return with_status(
                            warp::reply::json(&"Cannot_identify_your_ip"),
                            StatusCode::BAD_REQUEST,
                        );
                    }
                };
                let timestamp = Instant::now();
                state
                    .inner
                    .write()
                    .await
                    .insert((name.clone(), body.tag.clone()), timestamp);
                info!("register {:?} {:?}", name, body.tag);
                let pair = (name.clone(), body.tag);
                tokio::spawn(async move {
                    tokio::time::sleep(body.keepalive).await;
                    let mut lock = state.inner.write().await;
                    if let Some(ts) = lock.get(&pair) {
                        if *ts == timestamp {
                            lock.remove(&pair);
                            info!("invalidate {:?} {:?}", pair.0, pair.1);
                        } else {
                            trace!("survice {:?} {:?}", pair.0, pair.1);
                        }
                    }
                });
                with_status(warp::reply::json(&name), StatusCode::OK)
            },
        );

    let remove = state
        .clone()
        .and(warp::delete())
        .and(warp::body::json())
        .and(warp::filters::addr::remote())
        .then(
            |state: Server, body: sd::RegisterRequest, addr: Option<SocketAddr>| async move {
                let name = match (body.name, addr) {
                    (Some(name), _) => name,
                    (None, Some(addr)) => sd::Name::Addr(addr),
                    (None, None) => {
                        return with_status(
                            warp::reply::json(&"Cannot_identify_your_ip"),
                            StatusCode::BAD_REQUEST,
                        )
                    }
                };
                if state
                    .inner
                    .write()
                    .await
                    .remove(&(name.clone(), body.tag.clone()))
                    .is_none()
                {
                    warn!("Try removing empty entry {:?} {:?}", name, body.tag);
                } else {
                    info!("Try removing empty entry {:?} {:?}", name, body.tag);
                }
                with_status(warp::reply::json(&name), StatusCode::OK)
            },
        );

    let route = query.or(register).or(remove);
    info!("establish server");
    warp::serve(route).run(opts.addr.unwrap()).await;
}
