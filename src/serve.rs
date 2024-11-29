mod service;
mod state;
mod livereload;

use crate::serve::{service::ServeService, state::State};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use std::{env::var, net::SocketAddr, time::Duration};
use tokio::{
    net::TcpListener,
    signal,
    sync::mpsc::{channel, Sender as MPSCSender},
    sync::broadcast::Sender as BCSender,
    task::JoinHandle,
};
use tokio::task::JoinSet;

//from https://github.com/tokio-rs/axum/blob/main/examples/graceful-shutdown/src/main.rs
async fn shutdown_signal(stop: Option<(JoinHandle<()>, MPSCSender<()>)>, stop_state: BCSender<()>) {
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

    let _ = stop_state.send(());
    if let Some((handle, send)) = stop {
        send.send(())
            .await
            .expect("unable to send stop signal to reloader thread");
        handle.await.expect("error in reloader thread");
    }
}

pub async fn serve() -> color_eyre::Result<()> {
    let port = var("PORT").expect("expected env var PORT");
    let addr: SocketAddr = format!("0.0.0.0:{port}")
        .parse()
        .expect("expected valid socket address to result from env var PORT");

    let (state, send_stop) = State::new().await?.expect("empty bucket");

    let reload = if state.tigris_token.is_none() {
        let (send_stop, mut recv_stop) = channel(1);
        let reload_state = state.clone();
        Some((
            tokio::task::spawn(async move {
                loop {
                    tokio::select! {
                        _ = recv_stop.recv() => {
                            info!("Stop signal received for saver");
                            break;
                        },
                        () = tokio::time::sleep(Duration::from_secs(60)) => {
                            info!("Reloading from timer");
                            if let Err(e) = reload_state.reload().await {
                                error!(?e, "Error reloading state");
                            }
                        }
                    }
                }
            }),
            send_stop,
        ))
    } else {
        None
    };

    let http = http1::Builder::new();
    let mut signal = std::pin::pin!(shutdown_signal(reload, send_stop));

    let svc = ServeService::new(state);

    let listener = TcpListener::bind(&addr).await?;
    info!(?addr, "Serving");

    let mut futures = JoinSet::new();

    loop {
        tokio::select! {
            Ok((stream, _addr)) = listener.accept() => {
                let io = TokioIo::new(stream);
                let svc = svc.clone();

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
            eprintln!("all connections gracefully closed");
        },
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            eprintln!("timed out wait for all connections to close");
        }
    }

    Ok(())
}
