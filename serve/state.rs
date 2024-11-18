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
    cache: Cache<String, (Vec<u8>, String)>,
    not_found: Option<(Vec<u8>, String)>,
}

impl State {
    pub async fn new () -> color_eyre::Result<Option<Self>> {
        async fn read_file (path: String, bucket: &Box<Bucket>) -> color_eyre::Result<(Vec<u8>, String, String)> {
            let contents = bucket.get_object(&path).await?;
            let headers = contents.headers();

            let Some(content_type) = headers.get("content-type") else {
                bail!("unable to get CONTENT_TYPE");
            };
            let bytes = contents.to_vec();
            trace!(?path, len=?bytes.len(), ?content_type, "Read in file from S3");

            Ok((bytes, content_type.to_owned(), path))
        }

        let bucket = get_bucket();
        let Some(upload_data) = get_upload_data(&bucket).await? else {
            return Ok(None);
        };
        info!("Got bucket & upload data");

        let cache = Cache::new(1024);
        let mut read_files: FuturesUnordered<_> = upload_data.entries.keys().filter_map(|x| x.to_str())
            .map(|pb| read_file(pb.to_string(), &bucket)).collect();

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
            upload_data,
            cache,
            not_found
        }))
    }

    pub async fn get (&self, path: &str) -> Option<(Vec<u8>, String)> {
        self.cache.get(path).await
    }

    pub fn not_found (&self) -> Option<(Vec<u8>, String)> {
        self.not_found.clone()
    }

    pub fn file_root_dir (&self) -> PathBuf {
        self.upload_data.root.clone()
    }
}