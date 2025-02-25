use crate::{cache_control::cache, protect::protect, serve::serve, upload::upload};
use color_eyre::owo_colors::OwoColorize;
use dialoguer::{theme::Theme, FuzzySelect, Input};
use dotenvy::var;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    env::args,
    fmt::{Display, Formatter},
    hash::{Hash, Hasher},
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub fn hash_raw_bytes(bytes: impl AsRef<[u8]>) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hasher.finalize().to_vec()
}

pub mod cache_control;
mod non_empty_list;
pub mod protect;
pub mod s3;
pub mod serve;
mod upload;

#[macro_use]
extern crate tracing;

extern crate serde_regex;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Realm {
    StartsWith(String),
    #[serde(with = "serde_regex")]
    Regex(Regex),
    EndsWith(String),
    Contains(String),
}

impl Display for Realm {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Realm::StartsWith(sw) => write!(f, "Starts with: {sw:?}"),
            Realm::EndsWith(ew) => write!(f, "Ends with: {ew:?}"),
            Realm::Regex(regex) => write!(f, "Matches Regex: {regex}"),
            Realm::Contains(cont) => write!(f, "Contains: {cont:?}"),
        }
    }
}

impl Realm {
    pub fn matches(&self, path: &str) -> bool {
        match self {
            Self::StartsWith(pattern) => path.starts_with(pattern),
            Self::EndsWith(ew) => path.ends_with(ew),
            Self::Regex(regex) => regex.is_match(path),
            Self::Contains(cont) => path.contains(cont),
        }
    }

    pub fn get_from_stdin(theme: &dyn Theme) -> color_eyre::Result<Self> {
        let ty = FuzzySelect::with_theme(theme)
            .items(&["Starts With", "Ends With", "Regex", "Contains"])
            .with_prompt("What kind of realm matcher?")
            .interact()?;

        match ty {
            0 => {
                let sw = Input::with_theme(theme)
                    .with_prompt("What should the path start with?")
                    .interact()?;
                Ok(Self::StartsWith(sw))
            }
            1 => {
                let ew = Input::with_theme(theme)
                    .with_prompt("What should the path end with?")
                    .interact()?;
                Ok(Self::StartsWith(ew))
            }
            2 => {
                let regex = Input::with_theme(theme)
                    .with_prompt("What should the regular expression match on?")
                    .interact()?;
                Ok(Self::Regex(regex))
            }
            3 => {
                let ew = Input::with_theme(theme)
                    .with_prompt("What should the path contain?")
                    .interact()?;
                Ok(Self::Contains(ew))
            }
            _ => unreachable!(),
        }
    }
}

impl Hash for Realm {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Realm::StartsWith(sw) => sw.hash(state),
            Realm::EndsWith(ew) => ew.hash(state),
            Realm::Regex(reg) => reg.as_str().hash(state),
            Realm::Contains(cont) => cont.hash(state),
        }
    }
}

impl PartialEq for Realm {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Realm::StartsWith(s), Realm::StartsWith(o)) => s.eq(o),
            (Realm::EndsWith(s), Realm::EndsWith(o)) => s.eq(o),
            //technically not comprehensive but i'm not dealing with that mess lolll
            //also that would break the hash/partialeq invariant
            (Realm::Regex(s), Realm::Regex(o)) => s.as_str().eq(o.as_str()),
            (Realm::Contains(s), Realm::Contains(o)) => s.eq(o),
            //could technically turn the sw/ew into a regex, but again, no
            //that'd also break the hash/partialeq invariant
            (_, _) => false,
        }
    }
}

impl Eq for Realm {}

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

    if let Err(e) = dotenvy::dotenv() {
        eprintln!("Error finding env vars: {e:?}")
    }

    let sub = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(EnvFilter::from_default_env());

    if var("SENTRY_DSN").is_ok() {
        sub.with(sentry::integrations::tracing::layer()).init();
    } else {
        sub.init();
    }

    color_eyre::install().expect("unable to install color-eyre");
}

pub enum Args {
    Serve,
    Upload(String),
    Protect,
    Cache,
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
                "cache" => {
                    return Self::Cache;
                }
                _ => {}
            }
        }

        //could do it all in one, but this way is easier if i want colours
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
        eprintln!("- {}", "cache".italic());
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
        eprintln!("`{}` command", "cache".italic());
        eprintln!("  Modifies the cache control headers on files",);
        eprintln!("  eg. `{}`", "shove cache".cyan());
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
            "{} - the port used for serving the bucket. Not needed if uploading/protecting. Defaults to 8080",
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
        Args::Cache => runtime.block_on(async move {
            if let Err(e) = cache().await {
                error!(?e, "Error caching");
            }
        }),
    }
}
