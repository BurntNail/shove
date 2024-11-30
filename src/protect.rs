use crate::{protect::auth::AuthChecker, s3::get_bucket};

pub mod auth;

pub async fn protect(
    pattern: String,
    username: String,
    password: String,
) -> color_eyre::Result<()> {
    let bucket = get_bucket();
    let mut existing_auth = AuthChecker::new(&bucket).await?;
    existing_auth.protect(pattern, username, password).await?;
    existing_auth.save(&bucket).await?;

    Ok(())
}
