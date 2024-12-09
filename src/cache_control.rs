use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, RwLock};
use hyper::header::CACHE_CONTROL;
use s3::Bucket;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use crate::{hash_raw_bytes, Realm};
use std::fmt::Write;
use color_eyre::eyre::bail;
use crate::s3::get_bytes_or_default;

const CC_LOCATION: &str = "cache_control.json";

#[derive(Copy, Clone, Debug)]
pub enum Directive {
    MaxAge(usize),
    NoCache,
    MustRevalidate,
    NoStore,
    StaleWhileRevalidate,
}

impl Display for Directive {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Directive::MaxAge(secs) => write!(f, "max-age={secs}"),
            Directive::NoCache => write!(f, "no-cache"),
            Directive::MustRevalidate => write!(f, "must-revalidate"),
            Directive::NoStore => write!(f, "no-store"),
            Directive::StaleWhileRevalidate => write!(f, "stale-while-revalidate")
        }
    }
}

impl Directive {
    pub fn to_header (directives: Vec<Directive>) -> String {
        let mut output = format!("{}: ", CACHE_CONTROL);

        let mut is_first = true;
        for directive in directives {
            if is_first {
                is_first = false;
            } else if let Err(e) = write!(output, ", ") {
                error!(?e, "Error writing comma to cache control header");
            }

            if let Err(e) = write!(output, "{directive}") {
                error!(?e, "Error writing {directive:?} to cache control header");
            }
        }

        output
    }
}

#[derive(Debug, Clone)]
pub struct CacheControlManager {
    last_hash: Arc<Mutex<Vec<u8>>>,
    current: Arc<RwLock<Caching>>,
}

impl CacheControlManager {
    pub async fn new (bucket: &Bucket) -> color_eyre::Result<Self> {
        let (caching, raw_bytes) = Caching::new(bucket).await?;
        let hashed_bytes = hash_raw_bytes(&raw_bytes);

        Ok(Self {
            last_hash: Arc::new(Mutex::new(hashed_bytes)),
            current: Arc::new(RwLock::new(caching))
        })
    }

    pub async fn reload (&self, bucket: &Bucket) -> color_eyre::Result<()> {
        let Ok(mut last_hash) = self.last_hash.try_lock() else {
            bail!("already reloading cache control")
        };

        let raw_bytes = Caching::get_raw_bytes(bucket).await?;
        if raw_bytes.is_empty() {
            return Ok(());
        }

        let new_hash = hash_raw_bytes(&raw_bytes);

        if *last_hash == new_hash {
            return Ok(());
        }

        let new_version = Caching::construct_from_bytes(&raw_bytes)?;
        *self.current.write() = new_version;

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Caching {
    pub default: Option<Directive>,
    pub overrides: HashMap<Realm, Directive>
}

impl Caching {
    pub async fn new (bucket: &Bucket) -> color_eyre::Result<(Self, Vec<u8>)> {
        let bytes = Self::get_raw_bytes(bucket).await?;
        let s = Self::construct_from_bytes(&bytes)?;
        Ok((s, bytes))
    }

    async fn get_raw_bytes (bucket: &Bucket) -> color_eyre::Result<Vec<u8>> {
        get_bytes_or_default(bucket, CC_LOCATION).await
    }

    //not very necessary rn, but good for API footprint stuff later
    fn construct_from_bytes (bytes: &[u8]) -> color_eyre::Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }

        Ok(serde_json::from_slice(bytes)?)
    }
}