use crate::{
    hash_raw_bytes, s3::UPLOAD_DATA_LOCATION, serve::livereload::LiveReloader, UploadData,
};
use color_eyre::eyre::bail;
use futures::{stream::FuturesUnordered, StreamExt};
use hyper::StatusCode;
use moka::future::{Cache, CacheBuilder};
use s3::{error::S3Error, Bucket};
use serde_json::from_slice;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::{Mutex, RwLock};

#[derive(Clone)]
pub struct Pages {
    upload_data: Arc<RwLock<UploadData>>,
    last_upload_hash: Arc<Mutex<Vec<u8>>>,
    cache: Cache<String, (Vec<u8>, String)>,
}

impl Pages {
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

    pub async fn new(bucket: &Bucket) -> color_eyre::Result<Option<Self>> {
        let (upload_data, hash) = {
            let data = bucket.get_object(UPLOAD_DATA_LOCATION).await;
            match data {
                Ok(data) => {
                    let bytes = data.bytes();
                    let ud: UploadData = from_slice(bytes)?;
                    let hash = hash_raw_bytes(bytes);
                    (ud, hash)
                }
                Err(e) => {
                    return match e {
                        S3Error::HttpFailWithBody(404, _) => Ok(None),
                        _ => Err(e.into()),
                    }
                }
            }
        };

        let cache = CacheBuilder::new(256)
            .support_invalidation_closures()
            .build();

        match Self::read_file_from_s3(format!("{}/404.html", &upload_data.root), bucket).await {
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
            upload_data: Arc::new(RwLock::new(upload_data)),
            last_upload_hash: Arc::new(Mutex::new(hash)),
            cache,
        }))
    }

    pub async fn check_and_reload(
        &self,
        bucket: &Bucket,
        reloader: LiveReloader,
    ) -> color_eyre::Result<()> {
        let Ok(last_upload_hash) = self.last_upload_hash.try_lock() else {
            bail!("Already reloading");
        };

        let (bytes, hash) = {
            let rsp = bucket.get_object(UPLOAD_DATA_LOCATION).await?;
            let bytes = rsp.to_vec();
            let hash = hash_raw_bytes(&bytes);
            (bytes, hash)
        };

        if *last_upload_hash == hash {
            return Ok(());
        }

        let old_upload_data = self.upload_data.read().await.clone();
        let new_upload_data: UploadData = from_slice(&bytes)?;

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
        let task_bucket = bucket.clone();
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
            if let Err(e) = reloader.send_reload().await {
                error!(?e, "Error reloading tasks");
            }
        });

        Ok(())
    }

    pub async fn get(&self, bucket: &Bucket, path: &str) -> Option<(Vec<u8>, String, StatusCode)> {
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
            Some(_hash) => match Self::read_file_from_s3(path.clone(), bucket).await {
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
}
