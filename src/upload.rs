use crate::{
    s3::{get_bucket, get_upload_data},
};
use crate::upload::machinery::upload_dir_to_bucket;

mod machinery;

pub async fn upload(dir: &str) -> color_eyre::Result<()> {
    info!(?dir, "Reading files");

    let bucket = get_bucket();
    let current_upload_data = get_upload_data(&bucket).await?;
    upload_dir_to_bucket(dir, &bucket, current_upload_data).await?;

    Ok(())
}
