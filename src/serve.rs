mod livereload;
mod service;
mod state;

use crate::serve::{livereload::LiveReloader, service::ServeService, state::State};
use http_body_util::Full;
use hyper::{body::Bytes, http, server::conn::http1, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::{env::var, net::SocketAddr, time::Duration};
use tokio::{
    net::TcpListener,
    signal,
    sync::mpsc::{channel, Sender as MPSCSender},
    task::{JoinHandle, JoinSet},
};

enum Reloader {
    Interval(JoinHandle<()>, MPSCSender<()>),
    Waiting(LiveReloader),
}

pub fn empty_with_code(code: StatusCode) -> Result<Response<Full<Bytes>>, http::Error> {
    Response::builder().status(code).body(Full::default())
}

//from https://github.com/tokio-rs/axum/blob/main/examples/graceful-shutdown/src/main.rs
async fn shutdown_signal(reload_stop: Reloader) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    match reload_stop {
        Reloader::Interval(handle, send) => {
            let _ = send.send(()).await;
            if let Err(e) = handle.await {
                error!(?e, "Error awaiting for reload thread handle");
            }
        }
        Reloader::Waiting(livereload) => {
            if let Err(e) = livereload.send_stop().await {
                error!(?e, "Error stopping live reloader");
            }
        }
    }
}

pub async fn serve() -> color_eyre::Result<()> {
    let port = var("PORT").expect("expected env var PORT");
    let addr: SocketAddr = format!("0.0.0.0:{port}")
        .parse()
        .expect("expected valid socket address to result from env var PORT");

    let state = State::new().await?.expect("empty bucket");

    let reload = if state.tigris_token.is_none() {
        let (send_stop, mut recv_stop) = channel(1);
        let reload_state = state.clone();
        Reloader::Interval(
            tokio::task::spawn(async move {
                loop {
                    tokio::select! {
                        _ = recv_stop.recv() => {
                            info!("Stop signal received for saver");
                            break;
                        },
                        () = tokio::time::sleep(Duration::from_secs(60)) => {
                            info!("Reloading from timer");
                            if let Err(e) = reload_state.check_and_reload().await {
                                error!(?e, "Error reloading state");
                            }
                        }
                    }
                }
            }),
            send_stop,
        )
    } else {
        Reloader::Waiting(state.live_reloader())
    };

    let http = http1::Builder::new();
    let mut signal = std::pin::pin!(shutdown_signal(reload));

    let listener = TcpListener::bind(&addr).await?;
    info!(?addr, "Serving");

    let mut futures = JoinSet::new();

    loop {
        tokio::select! {
            Ok((stream, remote_addr)) = listener.accept() => {
                let io = TokioIo::new(stream);
                let svc = ServeService::new(state.clone(), remote_addr);

                let conn = http.serve_connection(io, svc).with_upgrades();

                futures.spawn(async move {
                    if let Err(e) = conn
                        .await {
                        error!(?e, "Error serving request");
                    }
                });
            },
            _ = &mut signal => {
                warn!("Graceful shutdown received");
                break;
            }
        }
    }

    tokio::select! {
        _ = futures.shutdown() => {
            info!("all connections gracefully closed");
        },
        _ = tokio::time::sleep(Duration::from_secs(10)) => {
            error!("timed out wait for all connections to close");
        }
    }

    Ok(())
}
