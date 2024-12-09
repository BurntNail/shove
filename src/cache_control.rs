use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc};
use tokio::sync::RwLock;
use s3::Bucket;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use crate::{hash_raw_bytes, Realm};
use std::fmt::Write;
use color_eyre::eyre::bail;
use crate::s3::get_bytes_or_default;

const CC_LOCATION: &str = "cache_control.json";

#[derive(Serialize, Deserialize, Copy, Clone, Debug)]
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
    pub fn directives_to_header(directives: Vec<Directive>) -> Option<String> {
        if directives.is_empty() {
            return None;
        }

        let mut output = String::default();

        let mut is_first = true;
        for directive in directives {
            if is_first {
                is_first = false;
            } else if let Err(e) = write!(output, ", ") {
                error!(?e, "Error writing comma to cache control header");
                return None;
            }

            if let Err(e) = write!(output, "{directive}") {
                error!(?e, "Error writing {directive:?} to cache control header");
                return None;
            }
        }

        Some(output)
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
        *last_hash = new_hash;

        let new_version = Caching::construct_from_bytes(&raw_bytes)?;
        *self.current.write().await = new_version;

        Ok(())
    }

    pub async fn get_directives (&self, path: &str) -> Vec<Directive> {
        self.current.read().await.get_cache_control_directives(path).await
    }
}

#[derive(Debug, Clone, Default)]
pub struct Caching {
    default: Option<Directive>,
    overrides: HashMap<Realm, Vec<Directive>>
}

#[derive(Serialize, Deserialize)]
pub struct StoredCaching {
    default: Option<Directive>,
    overrides: Vec<(Realm, Vec<Directive>)>
}

impl From<Caching> for StoredCaching {
    fn from(value: Caching) -> Self {
        Self {
            default: value.default,
            overrides: value.overrides.into_iter().collect()
        }
    }
}
impl From<StoredCaching> for Caching {
    fn from(value: StoredCaching) -> Self {
        Self {
            default: value.default,
            overrides: value.overrides.into_iter().collect()
        }
    }
}

impl Caching {
    pub async fn new (bucket: &Bucket) -> color_eyre::Result<(Self, Vec<u8>)> {
        let bytes = Self::get_raw_bytes(bucket).await?;
        let s = Self::construct_from_bytes(&bytes)?;
        Ok((s, bytes))
    }

    pub async fn save (&self, bucket: &Bucket) -> color_eyre::Result<()> {
        let stored: StoredCaching = self.clone().into(); //can't do ref stuff because we have to do in-memory stuff for the hashmap :(
        let bytes = serde_json::to_vec(&stored)?;

        bucket.put_object_with_content_type(CC_LOCATION, &bytes, "application/json").await?;

        Ok(())
    }

    async fn get_raw_bytes (bucket: &Bucket) -> color_eyre::Result<Vec<u8>> {
        get_bytes_or_default(bucket, CC_LOCATION).await
    }

    //not very necessary rn, but good for API footprint stuff later
    fn construct_from_bytes (bytes: &[u8]) -> color_eyre::Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        let stored: StoredCaching = serde_json::from_slice(bytes)?;
        Ok(stored.into())
    }

    pub async fn get_cache_control_directives (&self, path: &str) -> Vec<Directive> {
        let mut from_map: Vec<Directive> = self.overrides.iter()
            .filter(|(realm, _)| realm.matches(path))
            .flat_map(|(_, dirs)| dirs.clone())
            .collect();

        if from_map.is_empty() {
            if let Some(default) = self.default {
                from_map.push(default);
            }
        }

        from_map
    }
}