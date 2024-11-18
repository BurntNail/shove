mod state;
mod service;

use std::env::var;
use tokio::net::TcpListener;
use bloggthingie::setup;
use crate::state::State;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use crate::service::ServeService;

#[macro_use]
extern crate tracing;

#[tokio::main]
async fn main () -> color_eyre::Result<()> {
    setup();

    let port = var("PORT").expect("expected env var PORT");
    let addr = format!("0.0.0.0:{port}");
    let state = State::new().await?.expect("empty bucket");

    let svc = ServeService {
        state
    };

    let listener = TcpListener::bind(&addr).await?;
    info!(?addr, "Serving");

    loop {
        let (stream, _) = listener.accept().await?;

        let io = TokioIo::new(stream);
        let svc = svc.clone();

        tokio::task::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, svc)
                .await {
                error!(?e, "Error serving request");
            }
        });
    }

    Ok(())
}