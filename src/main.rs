use crate::{protect::protect, serve::serve, upload::upload};
use dotenvy::var;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env::{args},
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use color_eyre::owo_colors::OwoColorize;

pub mod protect;
pub mod s3;
pub mod serve;
mod upload;

#[macro_use]
extern crate tracing;

#[derive(Serialize, Deserialize, Default, Debug, Clone, Eq, PartialEq)]
pub struct UploadData {
    ///path to hash
    pub entries: HashMap<String, String>,
    pub root: String,
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
    }

    match dotenvy::dotenv() {
        Ok(file) => println!("Found env vars: {file:?}"),
        Err(e) => eprintln!("Error finding env vars: {e:?}"),
    }

    let sub = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(EnvFilter::from_default_env());

    if var("SENTRY_DSN").is_ok() {
        sub.with(sentry::integrations::tracing::layer()).init()
    } else {
        sub.init();
    }

    color_eyre::install().expect("unable to install color-eyre");
}

pub enum Args {
    Serve,
    Upload(String),
    Protect,
}

impl Args {
    pub fn parse() -> Self {
        let mut args = args().skip(1);

        if let Some(command) = args.next() {
            match command.as_str() {
                "serve" => {
                    return Self::Serve;
                }
                "upload" => {
                    if let Some(dir) = args.next() {
                        return Self::Upload(dir);
                    } else {
                        eprintln!("missing argument {}", "[DIR]".blue());
                        std::process::exit(1);
                    }
                }
                "protect" => {
                    return Self::Protect;
                }
                _ => {}
            }
        }

        eprintln!(
            "{} is a command-line utility to upload to and serve from S3 buckets",
            "shove".bold()
        );
        eprintln!(
            "All source code can be found at {}",
            "https://github.com/BurntNail/shove".underline()
        );
        eprintln!();
        eprintln!("Usage: {} [command]", "shove".bold());
        eprintln!();
        eprintln!("{}", "Available Commands:".underline());
        eprintln!("- {}", "serve".italic());
        eprintln!("- {} {}", "upload".italic(), "[DIR]".blue());
        eprintln!("- {}", "protect".italic());
        eprintln!();
        eprintln!("`{}` command", "serve".italic());
        eprintln!(
            "  Serves the provided {} on the provided {}",
            "S3_BUCKET".green(),
            "PORT".green()
        );
        eprintln!("  eg. `{}`", "shove serve".cyan());
        eprintln!();
        eprintln!("`{}` command", "upload".italic());
        eprintln!(
            "  Takes in a {}, which must be a valid directory other than the current directory",
            "DIR".blue()
        );
        eprintln!(
            "  Uploads {} to the provided {}",
            "DIR".blue(),
            "S3_BUCKET".green()
        );
        eprintln!("  eg. `{}`", "shove upload public".cyan());
        eprintln!();
        eprintln!("`{}` command", "protect".italic());
        eprintln!(
            "  Asks the user for a directory to protect, and the username/password combo to protect it",
        );
        eprintln!("  eg. `{}`", "shove protect".cyan());
        eprintln!();
        eprintln!("{}", "Environment Variables".underline());
        eprintln!(
            "{} - the secret key ID for the S3 bucket",
            "AWS_ACCESS_KEY_ID".green()
        );
        eprintln!(
            "{} - the secret access key for the S3 bucket",
            "AWS_SECRET_ACCESS_KEY".green()
        );
        eprintln!("{} - the name of the S3 bucket", "S3_BUCKET".green());
        eprintln!(
            "{} - the endpoint of the S3 bucket",
            "AWS_ENDPOINT_URL_S3".green()
        );
        eprintln!(
            "{} - the port used for serving the bucket. Not needed if uploading/protecting",
            "PORT".green()
        );
        eprintln!(
            "{} - the sentry DSN for use with analytics. Not needed if uploading/protecting. Optional",
            "SENTRY_DSN".green()
        );
        eprintln!(
            "{} - the key used to encrypt the authentication data. Not needed if uploading.",
            "AUTH_ENCRYPTION_KEY".green(),
        );
        eprintln!("{} - the authentication token for use with Tigris Webhooks. Not needed if uploading/protecting. Optional", "TIGRIS_TOKEN".green());

        std::process::exit(1);
    }
}

fn main() {
    let args = Args::parse();
    setup();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("unable to build runtime");

    match args {
        Args::Serve => {
            let dsn = match var("SENTRY_DSN") {
                Ok(x) => match x.parse() {
                    Ok(x) => Some(x),
                    Err(e) => {
                        warn!(?e, "Error parsing sentry DSN");
                        None
                    }
                },
                Err(_) => {
                    warn!("No Sentry DSN detected");
                    None
                }
            };

            let _sentry = sentry::init(sentry::ClientOptions {
                dsn,
                release: sentry::release_name!(),
                traces_sample_rate: 0.1,
                ..Default::default()
            });
            runtime.block_on(async move {
                if let Err(e) = serve().await {
                    error!(?e, "Error serving");
                }
            });
        }
        Args::Upload(dir) => runtime.block_on(async move {
            if let Err(e) = upload(&dir).await {
                error!(?e, "Error uploading");
            }
        }),
        Args::Protect => {
            runtime.block_on(async move {
                if let Err(e) = protect().await {
                    error!(?e, "Error protecting");
                }
            });
        }
    }
}
