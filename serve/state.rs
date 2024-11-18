use std::path::PathBuf;
use color_eyre::eyre::bail;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use moka::future::Cache;
use s3::Bucket;
use bloggthingie::aws::{get_bucket, get_upload_data};
use bloggthingie::UploadData;

#[derive(Clone, Debug)]
pub struct State {
    bucket: Box<Bucket>,
    upload_data: UploadData,
    cache: Cache<PathBuf, (Vec<u8>, String)>,
}

impl State {
    pub async fn new () -> color_eyre::Result<Option<Self>> {
        async fn read_file (path: &PathBuf, bucket: &Box<Bucket>) -> color_eyre::Result<(Vec<u8>, String, PathBuf)> {
            let contents = bucket.get_object(path.to_str().unwrap()).await?;
            let headers = contents.headers();

            let Some(content_type) = headers.get("content-type") else {
                bail!("unable to get CONTENT_TYPE");
            };
            let bytes = contents.to_vec();
            trace!(?path, len=?bytes.len(), ?content_type, "Read in file from S3");

            Ok((bytes, content_type.to_owned(), path.to_owned()))
        }

        let bucket = get_bucket();
        let Some(upload_data) = get_upload_data(&bucket).await? else {
            return Ok(None);
        };
        info!("Got bucket & upload data");

        let cache = Cache::new(1024);
        let mut read_files: FuturesUnordered<_> = upload_data.entries.keys().map(|pb| read_file(pb, &bucket)).collect();

        while let Some(res) = read_files.next().await {
            let (contents, content_type, pathbuf) = res?;
            cache.insert(pathbuf, (contents, content_type)).await;
        }

        info!("Read files");

        drop(read_files);


        Ok(Some(Self {
            bucket,
            upload_data,
            cache,
        }))
    }
}