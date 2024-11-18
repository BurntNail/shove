mod service;
mod state;

use crate::{service::ServeService, state::State};
use bloggthingie::setup;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use std::{env::var, time::Duration};
use tokio::{
    net::TcpListener,
    signal,
    sync::mpsc::{channel, Sender},
    task::JoinHandle,
};

#[macro_use]
extern crate tracing;

//from https://github.com/tokio-rs/axum/blob/main/examples/graceful-shutdown/src/main.rs
async fn shutdown_signal(stop_send: Sender<()>, handle: JoinHandle<()>) {
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

    stop_send
        .send(())
        .await
        .expect("unable to send stop signal to reloader thread");
    handle.await.expect("error in reloader thread");
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    setup();

    let port = var("PORT").expect("expected env var PORT");
    let addr = format!("0.0.0.0:{port}");

    let state = State::new().await?.expect("empty bucket");

    let (send_stop, mut recv_stop) = channel(1);
    let reload_state = state.clone();
    let reload_handle = tokio::task::spawn(async move {
        loop {
            tokio::select! {
                _ = recv_stop.recv() => {
                    info!("Stop signal received for saver");
                    break;
                },
                () = tokio::time::sleep(Duration::from_secs(30)) => {
                    if let Err(e) = reload_state.reload().await {
                        error!(?e, "Error reloading state");
                    }
                }
            }
        }
    });

    let http = http1::Builder::new();
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    let mut signal = std::pin::pin!(shutdown_signal(send_stop, reload_handle));

    let svc = ServeService { state };

    let listener = TcpListener::bind(&addr).await?;
    info!(?addr, "Serving");

    loop {
        tokio::select! {
            Ok((stream, _addr)) = listener.accept() => {
                let io = TokioIo::new(stream);
                let svc = svc.clone();

                let conn = http.serve_connection(io, svc);
                let fut = graceful.watch(conn);

                tokio::task::spawn(async move {
                    if let Err(e) = fut
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
        _ = graceful.shutdown() => {
            eprintln!("all connections gracefully closed");
        },
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
            eprintln!("timed out wait for all connections to close");
        }
    }

    Ok(())
}
