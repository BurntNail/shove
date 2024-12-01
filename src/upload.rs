use crate::{
    s3::{get_bucket, get_upload_data},
    upload::machinery::upload_dir_to_bucket,
};
use color_eyre::owo_colors::OwoColorize;
use std::{env::current_dir, path::PathBuf};
use color_eyre::eyre::bail;

mod machinery;

pub async fn upload(dir: &str) -> color_eyre::Result<()> {
    let mut failed = false;

    let Ok(dir_path_buffer) = PathBuf::from(&dir).canonicalize() else {
        bail!("unable to canonicalise {}", "[DIR]".blue());
    };
    if !dir_path_buffer.exists() {
        eprintln!("unable to find provided {}", "[DIR]".blue());
        failed = true;
    }
    if !dir_path_buffer.is_dir() {
        eprintln!("provided {} must be a directory", "[DIR]".blue());
        failed = true;
    }
    match current_dir() {
        Ok(cd) => {
            if dir_path_buffer.eq(&cd) {
                eprintln!(
                    "provided {} must be a different from current directory",
                    "[DIR]".blue()
                );
                failed = true;
            }
        }
        Err(e) => {
            eprintln!("unable to access current directory: {e:?}");
            failed = true;
        }
    }

    if failed {
        std::process::exit(1);
    }

    info!(?dir, "Reading files");

    let bucket = get_bucket();
    let current_upload_data = get_upload_data(&bucket).await?;
    upload_dir_to_bucket(dir, &bucket, current_upload_data).await?;

    Ok(())
}
