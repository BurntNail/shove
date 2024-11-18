use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use crate::aws::{get_bucket, get_upload_data};
use crate::upload::upload_dir_to_bucket;

mod upload;
mod aws;

#[macro_use]
extern crate tracing;

#[derive(Serialize, Deserialize)]
pub struct UploadData {
    root: PathBuf,
    hash: String,
}

fn setup() {
    dotenvy::dotenv().unwrap();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();
    color_eyre::install().expect("unable to install color-eyre");

    if cfg!(debug_assertions) {
        const TO: &str = "full";
        for key in &["RUST_SPANTRACE", "RUST_LIB_BACKTRACE", "RUST_BACKTRACE"] {
            match std::env::var(key) {
                Err(_) => {
                    trace!(%key, %TO, "Setting env var");
                    std::env::set_var(key, "full");
                }
                Ok(found) => {
                    trace!(%key, %found, "Found existing env var");
                }
            }
        }
    }
}


#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    setup();
    let bucket = get_bucket();
    let current_upload_data = get_upload_data(&bucket).await?;
    upload_dir_to_bucket("examplepublic", &bucket, current_upload_data).await?;
    Ok(())
}