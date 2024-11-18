use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub mod aws;

#[macro_use]
extern crate tracing;

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct UploadData {
    ///path to hash
    pub entries: HashMap<PathBuf, String>,
    pub root: PathBuf,
}

pub fn setup() {
    if cfg!(debug_assertions) {
        for (key, value) in &[
            ("RUST_SPANTRACE", "full"),
            ("RUST_LIB_BACKTRACE", "full"),
            ("RUST_BACKTRACE", "full"),
            ("RUST_LOG", "info"),
        ] {
            match std::env::var(key) {
                Err(_) => {
                    trace!(%key, %value, "Setting env var");
                    std::env::set_var(key, value);
                }
                Ok(found) => {
                    trace!(%key, %found, "Found existing env var");
                }
            }
        }
        dotenvy::dotenv().unwrap();
    }


    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();
    color_eyre::install().expect("unable to install color-eyre");
}
