use crate::{
    protect::{
        auth::{AuthChecker, AuthReturn},
        auth_storer::AUTH_DATA_LOCATION,
    },
    s3::{get_bucket, get_upload_data},
    serve::livereload::LiveReloader,
    UploadData,
};
use color_eyre::eyre::bail;
use futures::{stream::FuturesUnordered, StreamExt};
use hyper::{body::Incoming, Request, StatusCode};
use moka::future::{Cache, CacheBuilder};
use s3::Bucket;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, env, net::SocketAddr, sync::Arc};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct State {
    bucket: Box<Bucket>,
    upload_data: Arc<RwLock<UploadData>>,
    last_auth_hash: Arc<RwLock<Vec<u8>>>,
    cache: Cache<String, (Vec<u8>, String)>,
    pub tigris_token: Option<Arc<str>>,
    live_reloader: LiveReloader,
    auth: AuthChecker,
}

impl State {
    #[instrument(skip(bucket))]
    async fn read_file_from_s3(
        path: String,
        bucket: &Bucket,
    ) -> color_eyre::Result<(Vec<u8>, String, String)> {
        let contents = bucket.get_object(&path).await?;
        let headers = contents.headers();

        let Some(content_type) = headers.get("content-type") else {
            bail!("unable to get CONTENT_TYPE");
        };
        let bytes = contents.to_vec();
        trace!(?path, len=?bytes.len(), ?content_type, "Read in file from S3");

        Ok((bytes, content_type.to_owned(), path))
    }

    #[instrument]
    pub async fn new() -> color_eyre::Result<Option<Self>> {
        let bucket = get_bucket();
        let Some(upload_data) = get_upload_data(&bucket).await? else {
            return Ok(None);
        };
        info!("Got bucket & upload data");

        let live_reloader = LiveReloader::new();
        let auth = AuthChecker::new(&bucket).await?;
        let raw_auth_hash = {
            let raw = match Self::read_file_from_s3(AUTH_DATA_LOCATION.to_string(), &bucket).await {
                Ok((x, _, _)) => x,
                Err(_e) => vec![],
            };

            let mut hasher = Sha256::new();
            hasher.update(&raw);
            hasher.finalize().to_vec()
        };
        let last_auth_hash = Arc::new(RwLock::new(raw_auth_hash));

        let cache = CacheBuilder::new(256)
            .support_invalidation_closures()
            .build();

        let tigris_token = env::var("TIGRIS_TOKEN").ok().map(|x| x.into());
        if tigris_token.is_some() {
            info!("Waiting on Tigris Webhook for reloads");
        } else {
            info!("Checking every 60s for reloads");
        }

        match Self::read_file_from_s3(format!("{}/404.html", &upload_data.root), &bucket).await {
            Ok((contents, content_type, path)) => {
                info!("Adding 404 path to cache");
                cache.insert(path, (contents, content_type)).await;
            }
            Err(e) => error!(?e, "Error getting 404 page from S3"),
        }

        let task_cache = cache.clone();
        let task_bucket = bucket.clone();
        let task_upload_data = upload_data.clone();
        tokio::task::spawn(async move {
            let mut read_files: FuturesUnordered<_> = task_upload_data
                .entries
                .keys()
                .map(|pb| Self::read_file_from_s3(pb.clone(), &task_bucket))
                .collect();

            while let Some(res) = read_files.next().await {
                match res {
                    Ok((contents, content_type, path)) => {
                        trace!(?path, "initial load adding to cache");
                        task_cache.insert(path, (contents, content_type)).await;
                    }
                    Err(e) => {
                        warn!(?e, "Error reading file from S3")
                    }
                }
            }

            info!("Read files from S3");
        });

        Ok(Some(Self {
            bucket,
            upload_data: Arc::new(RwLock::new(upload_data)),
            cache,
            tigris_token,
            live_reloader,
            auth,
            last_auth_hash,
        }))
    }

    pub fn live_reloader(&self) -> LiveReloader {
        self.live_reloader.clone()
    }

    #[instrument(skip(self))]
    pub async fn check_and_reload(&self) -> color_eyre::Result<()> {
        trace!("Checking for reload");

        let raw_auth_hash = {
            let raw =
                match Self::read_file_from_s3(AUTH_DATA_LOCATION.to_string(), &self.bucket).await {
                    Ok((x, _, _)) => x,
                    Err(_e) => vec![],
                };

            let mut hasher = Sha256::new();
            hasher.update(&raw);
            hasher.finalize().to_vec()
        };
        if raw_auth_hash.as_slice() != self.last_auth_hash.read().await.as_slice() {
            *self.last_auth_hash.write().await = raw_auth_hash;

            if let Err(e) = self.auth.reload(&self.bucket).await {
                error!(?e, "Error reloading auth checker");
            }
        }

        let old_upload_data = self.upload_data.read().await.clone();
        let Some(new_upload_data) = get_upload_data(&self.bucket).await? else {
            bail!("No upload data present");
        };

        if new_upload_data == old_upload_data {
            trace!("Same upload data, not changing");
            return Ok(());
        }

        info!("Reloading cache");

        *self.upload_data.write().await = new_upload_data.clone();

        let mut to_be_updated: HashSet<String> = new_upload_data.entries.keys().cloned().collect();
        let mut to_be_removed: Vec<String> = vec![];

        for (old_entry, old_hash) in old_upload_data.entries {
            match new_upload_data.entries.get(&old_entry) {
                Some(new_hash) => {
                    if &old_hash == new_hash {
                        to_be_updated.remove(&old_entry);
                    }
                }
                None => to_be_removed.push(old_entry),
            }
        }

        if let Err(e) = self
            .cache
            .invalidate_entries_if(move |entry, _| to_be_removed.contains(entry))
        {
            warn!(?e, "Error invalidating old entries")
        }

        let task_cache = self.cache.clone();
        let task_bucket = self.bucket.clone();
        let task_reload = self.live_reloader.clone();
        tokio::task::spawn(async move {
            let mut read_files: FuturesUnordered<_> = to_be_updated
                .into_iter()
                .map(|pb| Self::read_file_from_s3(pb.clone(), &task_bucket))
                .collect();

            while let Some(res) = read_files.next().await {
                match res {
                    Ok((contents, content_type, path)) => {
                        info!(?path, "file changed, updating");
                        task_cache.insert(path, (contents, content_type)).await;
                    }
                    Err(e) => {
                        warn!(?e, "Error updating file from S3")
                    }
                }
            }

            info!("Updated cache from S3");
            if let Err(e) = task_reload.send_reload().await {
                error!(?e, "Error reloading tasks");
            }
        });

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn get(&self, path: &str) -> Option<(Vec<u8>, String, StatusCode)> {
        let root = self.upload_data.read().await.clone().root;
        let path = format!("{root}{path}");

        let not_found = || async {
            let (content, content_type) = self.cache.get(&format!("{root}/404.html")).await?;
            Some((content, content_type, StatusCode::NOT_FOUND))
        };

        if let Some((c, ct)) = self.cache.get(&path).await {
            return Some((c, ct, StatusCode::OK));
        }

        match self.upload_data.read().await.entries.get(&path) {
            Some(_hash) => match Self::read_file_from_s3(path.clone(), &self.bucket).await {
                Ok((bytes, content_type, path)) => {
                    info!(?path, "Adding to cache");
                    self.cache
                        .insert(path, (bytes.clone(), content_type.clone()))
                        .await;
                    Some((bytes, content_type, StatusCode::OK))
                }
                Err(e) => {
                    warn!(
                        ?e,
                        "Error getting file from S3, removing from local upload data"
                    );
                    self.upload_data.write().await.entries.remove(&path);

                    not_found().await
                }
            },
            None => not_found().await,
        }
    }

    pub async fn check_auth(
        &self,
        path: &str,
        req: Request<Incoming>,
        remote_addr: SocketAddr,
    ) -> AuthReturn {
        self.auth.check_auth(path, req, remote_addr).await
    }
}
