use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub mod aws;

#[macro_use]
extern crate tracing;

#[derive(Serialize, Deserialize, Default, Debug, Clone, Eq, PartialEq)]
pub struct UploadData {
    ///path to hash
    pub entries: HashMap<String, String>,
    pub root: String,
}

pub fn setup<const SENTRY: bool>() {
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
    }

    match dotenvy::dotenv() {
        Ok(file) => println!("Found env vars: {file:?}"),
        Err(e) => eprintln!("Error finding env vars: {e:?}"),
    }

    let sub = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(EnvFilter::from_default_env());

    if SENTRY {
        sub
            .with(sentry::integrations::tracing::layer())
            .init()
    } else {
        sub.init();
    }

    color_eyre::install().expect("unable to install color-eyre");
}
