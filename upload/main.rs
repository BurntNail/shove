use crate::upload::upload_dir_to_bucket;
use bloggthingie::{
    aws::{get_bucket, get_upload_data},
    setup,
};
use std::env::args;

#[macro_use]
extern crate tracing;

mod upload;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    setup();

    let dir = args().nth(1).expect("usage: btupload [DIR]");

    let bucket = get_bucket();
    let current_upload_data = get_upload_data(&bucket).await?;
    upload_dir_to_bucket(&dir, &bucket, current_upload_data).await?;

    Ok(())
}