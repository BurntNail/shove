use std::path::PathBuf;
use blake2::{Blake2b512, Digest};
use new_mime_guess::MimeGuess;
use s3::Bucket;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use walkdir::WalkDir;
use crate::aws::UPLOAD_DATA_LOCATION;
use crate::UploadData;

struct Entry {
    pb: PathBuf,
    contents: Vec<u8>,
    mime_guess: MimeGuess
}

pub async fn upload_dir_to_bucket (dir: &str, bucket: &Box<Bucket>, existing: Option<UploadData>) -> color_eyre::Result<()> {
    let mut hasher = Blake2b512::new();
    let mut to_write = vec![];

    for item in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        let pb = item.path().to_path_buf();
        if !pb.is_file() {
            continue;
        }

        info!(?pb, "Reading file");

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

        hasher.update(&contents);

        to_write.push(Entry {
            pb,
            contents,
            mime_guess
        });
    }

    let finalised = hasher.finalize().to_vec();
    let string_finalised_hash: String = finalised.into_iter().map(|x| format!("{x:X}")).collect();

    if existing.is_some_and(|x| &x.hash == &string_finalised_hash) {
        info!("Found existing same hash");
        return Ok(());
    }

    let upload_data = UploadData {
        hash: string_finalised_hash,
        root: PathBuf::from(dir)
    };
    let json_upload_data = serde_json::to_vec(&upload_data)?;
    bucket.put_object_with_content_type(UPLOAD_DATA_LOCATION, &json_upload_data, mime::JSON.as_str()).await?;

    for Entry { pb, contents, mime_guess } in to_write {
        let content_type = mime_guess.first_or_octet_stream();
        let Some(path) = pb.to_str() else {
            error!(?pb, "unable to get string repr of path");
            continue
        };
        info!(?path, ?content_type, "Uploading to S3");
        bucket.put_object_with_content_type(path, &contents, content_type.essence_str()).await?;
    }

    info!("All uploaded to S3");


    Ok(())
}