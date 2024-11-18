mod state;

use std::env::var;
use axum::Router;
use axum::routing::get;
use tokio::net::TcpListener;
use bloggthingie::setup;
use crate::state::State;

#[macro_use]
extern crate tracing;

#[tokio::main]
async fn main () -> color_eyre::Result<()> {
    setup();

    let port = var("PORT").expect("expected env var PORT");
    let state = State::new().await?.expect("empty bucket");

    let app = Router::new()
        .route("/", get(|| async { "Hello, World!" }))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");

    let listener = TcpListener::bind(&addr).await?;
    info!(?addr, "Serving");
    axum::serve(listener, app).await?;

    Ok(())
}
