use crate::protect::auth::Auth;

mod auth;

pub async fn protect (pattern: String, username: String, password: String) -> color_eyre::Result<()> {
    let mut existing_auth = Auth::new().await?;


    todo!()
}