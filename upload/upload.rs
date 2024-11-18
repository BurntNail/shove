use blake2::{Blake2b512, Digest};
use bloggthingie::{aws::UPLOAD_DATA_LOCATION, UploadData};
use futures::{stream::FuturesUnordered, StreamExt};
use new_mime_guess::MimeGuess;
use s3::Bucket;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};
use color_eyre::eyre::bail;
use tokio::{fs::File, io::AsyncReadExt};
use walkdir::WalkDir;

struct Entry {
    path: String,
    contents: Vec<u8>,
    hash: String,
    mime_guess: MimeGuess,
}

pub async fn upload_dir_to_bucket(
    dir: &str,
    bucket: &Box<Bucket>,
    existing: Option<UploadData>,
) -> color_eyre::Result<()> {
    async fn read_file(pb: PathBuf) -> color_eyre::Result<Entry> {
        let Some(path) = pb.to_str().map(|x| x.to_string()) else {
            bail!("unable to get UTF-8 path")
        };

        trace!(?pb, "Reading file");

        let contents: Vec<u8> = {
            let mut file = File::open(&pb).await?;
            let mut contents = vec![];
            let mut tmp = [0_u8; 1024];
            loop {
                match file.read(&mut tmp).await? {
                    0 => break,
                    n => {
                        contents.extend(&tmp[0..n]);
                    }
                }
            }

            contents
        };

        let mime_guess = new_mime_guess::from_path(&pb);

        let mut hasher = Blake2b512::new();
        hasher.update(&contents);
        let hash = hasher.finalize().to_vec();
        let hash: String = hash.into_iter().map(|x| format!("{x:x}")).collect();

        info!(len=?contents.len(), ?pb, "Read file");

        Ok(Entry {
            path,
            contents,
            hash,
            mime_guess,
        })
    }
    async fn write_file_to_bucket(
        bucket: &Bucket,
        Entry {
            path,
            contents,
            hash: _,
            mime_guess,
        }: Entry,
    ) -> color_eyre::Result<()> {
        let content_type = mime_guess.first_or_octet_stream();
        let rsp = bucket
            .put_object_with_content_type(&path, &contents, content_type.essence_str())
            .await?;

        info!(?path, ?content_type, code=%rsp.status_code(), "Uploaded to S3");

        Ok(())
    }

    let UploadData {
        root: _,
        entries: existing_entries,
    } = existing.unwrap_or_default();

    info!("Reading files");
    let mut futures: FuturesUnordered<_> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|x| x.ok().filter(|x| x.path().is_file()))
        .map(|item| read_file(item.path().to_path_buf()))
        .collect();

    let mut to_write = vec![];
    let mut to_delete: HashSet<_> = existing_entries.keys().collect();
    let mut entries = HashMap::new();
    while let Some(entry) = futures.next().await {
        let entry = entry?;

        to_delete.remove(&entry.path);

        match existing_entries.get(&entry.path) {
            None => {
                entries.insert(entry.path.clone(), entry.hash.clone());
                to_write.push(entry);
            }
            Some(x) => {
                entries.insert(entry.path.clone(), entry.hash.clone());
                if x != &entry.hash {
                    to_write.push(entry);
                } else {
                    trace!(pb=?entry.path, "Skipping upload");
                }
            }
        }
    }

    info!("Read all files");

    let mut futures: FuturesUnordered<_> = to_write
        .into_iter()
        .map(|e| write_file_to_bucket(&bucket, e))
        .collect();
    while let Some(res) = futures.next().await {
        res?;
    }

    info!("Uploaded files to S3");

    let upload_data = UploadData {
        entries,
        root: PathBuf::from(dir),
    };
    let json_upload_data = serde_json::to_vec(&upload_data)?;
    bucket
        .put_object_with_content_type(UPLOAD_DATA_LOCATION, &json_upload_data, mime::JSON.as_str())
        .await?;

    info!("Uploaded object data to S3");


    for path in to_delete {
        info!(?path, "Deleting old file");
        bucket.delete_object(path).await?;
    }

    info!("Deleted old files from S3");

    Ok(())
}
