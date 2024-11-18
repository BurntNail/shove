use bloggthingie::{
    aws::{get_bucket, get_upload_data},
    UploadData,
};
use color_eyre::eyre::bail;
use futures::{stream::FuturesUnordered, StreamExt};
use moka::future::{Cache, CacheBuilder};
use s3::Bucket;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone, Debug)]
pub struct State {
    bucket: Box<Bucket>,
    upload_data: Arc<RwLock<UploadData>>,
    cache: Cache<String, (Vec<u8>, String)>,
    not_found: Arc<RwLock<Option<(Vec<u8>, String)>>>,
}

impl State {
    async fn read_file(
        path: String,
        bucket: &Box<Bucket>,
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

    pub async fn new() -> color_eyre::Result<Option<Self>> {
        let bucket = get_bucket();
        let Some(upload_data) = get_upload_data(&bucket).await? else {
            return Ok(None);
        };
        info!("Got bucket & upload data");

        let cache = CacheBuilder::new(1024).support_invalidation_closures().build();

        let mut read_files: FuturesUnordered<_> = upload_data
            .entries
            .keys()
            .filter_map(|x| x.to_str())
            .map(|pb| Self::read_file(pb.to_string(), &bucket))
            .collect();

        let mut not_found = None;

        while let Some(res) = read_files.next().await {
            let (contents, content_type, path) = res?;

            if path.contains("404.html") {
                not_found = Some((contents.clone(), content_type.clone()));
            }

            cache.insert(path, (contents, content_type)).await;
        }

        info!("Read files from S3");

        drop(read_files);

        Ok(Some(Self {
            bucket,
            upload_data: Arc::new(RwLock::new(upload_data)),
            cache,
            not_found: Arc::new(RwLock::new(not_found)),
        }))
    }

    pub async fn reload(&self) -> color_eyre::Result<()> {
        trace!("Checking for reload");

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

        let cloned_entries = new_upload_data.entries.clone();
        self.cache.invalidate_entries_if(move |key, _value| {
            !cloned_entries.contains_key(&PathBuf::from(key))
        })?;

        let mut read_files: FuturesUnordered<_> = new_upload_data
            .entries
            .keys()
            .filter_map(|x| x.to_str())
            .map(|pb| Self::read_file(pb.to_string(), &self.bucket))
            .collect();

        let mut not_found = None;

        while let Some(res) = read_files.next().await {
            let (contents, content_type, path) = res?;

            if path.contains("404.html") {
                not_found = Some((contents.clone(), content_type.clone()));
            }

            self.cache.insert(path, (contents, content_type)).await;
        }

        *self.not_found.write().await = not_found;

        info!("Finished reloading cache");

        Ok(())
    }

    pub async fn get(&self, path: &str) -> Option<(Vec<u8>, String)> {
        self.cache.get(path).await
    }

    pub async fn not_found(&self) -> Option<(Vec<u8>, String)> {
        self.not_found.read().await.clone()
    }

    pub async fn file_root_dir(&self) -> PathBuf {
        self.upload_data.read().await.clone().root
    }
}
