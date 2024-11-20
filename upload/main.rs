use crate::upload::upload_dir_to_bucket;
use shove::{
    aws::{get_bucket, get_upload_data},
    setup,
};
use std::env::{args, current_dir};
use std::path::PathBuf;
use color_eyre::eyre::bail;

#[macro_use]
extern crate tracing;

mod upload;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    setup::<false>();

    let dir = args().nth(1).expect("usage: shoveup [DIR]");

    let bucket = get_bucket();
    let current_upload_data = get_upload_data(&bucket).await?;
    upload_dir_to_bucket(&dir, &bucket, current_upload_data).await?;

    Ok(())
}
