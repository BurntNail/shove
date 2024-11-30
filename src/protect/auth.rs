use crate::serve::empty_with_code;
use base64::{prelude::BASE64_STANDARD, Engine};
use getrandom::getrandom;
use http_body_util::Full;
use hyper::{
    body::{Bytes, Incoming},
    http, Request, Response, StatusCode,
};
use s3::{error::S3Error, Bucket};
use serde::{Deserialize, Serialize};
use serde_json::from_slice;
use std::{collections::HashMap, sync::Arc};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::password_hash::{Error, SaltString};
use tokio::sync::RwLock;

pub const AUTH_DATA_LOCATION: &str = "auth_data.json";

#[derive(Clone)]
pub struct AuthChecker {
    //deliberately not using dashmap as I want to be able to replace the entire map
    entries: Arc<RwLock<HashMap<String, UsernameAndPassword>>>,
}

pub enum AuthReturn {
    AuthConfirmed(Request<Incoming>),
    ResponseFromAuth(Response<Full<Bytes>>),
    Error(http::Error),
}

impl From<Result<Response<Full<Bytes>>, http::Error>> for AuthReturn {
    fn from(value: Result<Response<Full<Bytes>>, http::Error>) -> Self {
        match value {
            Ok(x) => Self::ResponseFromAuth(x),
            Err(e) => Self::Error(e),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct UsernameAndPassword {
    username: String,
    stored_key: String,
}

impl AuthChecker {
    pub async fn new(bucket: &Bucket) -> color_eyre::Result<Self> {
        let entries = match bucket.get_object(AUTH_DATA_LOCATION).await {
            Ok(x) => from_slice(&x.to_vec()).ok(),
            Err(S3Error::HttpFailWithBody(404, _)) => None,
            Err(e) => return Err(e.into()),
        };

        let entries = Arc::new(RwLock::new(entries.unwrap_or_default()));

        Ok(Self { entries })
    }

    pub async fn rm_pattern(&self, pattern: &str) {
        let mut entries = self.entries.write().await;
        entries.remove(pattern);
    }

    pub async fn get_patterns_and_usernames (&self) -> Vec<(String, String)> {
        self.entries.read().await.iter().map(|(pat, uap)| {
            (pat.clone(), uap.username.clone())
        }).collect()
    }

    pub async fn reload(&self, bucket: &Bucket) -> color_eyre::Result<()> {
        let file_contents = bucket.get_object(AUTH_DATA_LOCATION).await?.to_vec();
        let entries = from_slice(&file_contents)?;

        *self.entries.write().await = entries;

        Ok(())
    }

    pub async fn save(self, bucket: &Bucket) -> color_eyre::Result<()> {
        let read_copy = self.entries.read().await;
        let sered = serde_json::to_vec(&*read_copy)?;
        bucket
            .put_object_with_content_type(AUTH_DATA_LOCATION, &sered, mime::JSON.as_str())
            .await?;

        Ok(())
    }

    pub async fn protect(
        &mut self,
        pattern: String,
        username: String,
        password: String,
    ) -> color_eyre::Result<()> {
        let mut salt = [0; 32];
        getrandom(&mut salt)?;
        let saltstring = SaltString::encode_b64(&salt)?;

        let argon2 = Argon2::default();
        let password_hash = argon2.hash_password(password.as_bytes(), &saltstring)?;
        let stored_key = password_hash.serialize().to_string();

        let mut writeable = self.entries.write().await;

        writeable.insert(
            pattern,
            UsernameAndPassword {
                username,
                stored_key,
            },
        );

        Ok(())
    }

    pub async fn check_auth(&self, path: &str, req: Request<Incoming>) -> AuthReturn {
        let readable = self.entries.read().await;
        let Some(UsernameAndPassword {
            username,
            stored_key,
        }) = readable
            .iter()
            .find(|(pattern, _)| path.contains(pattern.as_str()))
            .map(|(_, uap)| uap.clone())
        else {
            return AuthReturn::AuthConfirmed(req);
        };

        let password_hash = match PasswordHash::new(&stored_key) {
            Ok(x) => x,
            Err(e) => {
                error!(?e, "Unable to decode stoed password key");
                return empty_with_code(StatusCode::BAD_REQUEST).into();
            }
        };
        let argon = Argon2::default();

        let headers = req.headers();
        let provided_auth_b64 = match headers.get("Authorization").cloned() {
            Some(x) => match x.to_str() {
                Ok(x) => match x.strip_prefix("Basic ") {
                    Some(x) => x.to_string(),
                    None => {
                        debug!("Unable to find Basic part");
                        return empty_with_code(StatusCode::UNAUTHORIZED).into();
                    }
                },
                Err(e) => {
                    debug!(?e, "Error converting auth part to string");
                    return empty_with_code(StatusCode::BAD_REQUEST).into();
                }
            },
            None => {
                return Response::builder()
                    .header(
                        "WWW-Authenticate",
                        format!("Basic realm={path:?} charset=\"UTF-8\""),
                    )
                    .status(StatusCode::UNAUTHORIZED)
                    .body(Full::default())
                    .into();
            }
        };

        let decoded = match BASE64_STANDARD.decode(&provided_auth_b64) {
            Ok(dec) => match String::from_utf8(dec) {
                Ok(dec) => dec,
                Err(e) => {
                    debug!(?e, "Unable to turn decoded B64 BasicAuth into string");
                    return empty_with_code(StatusCode::BAD_REQUEST).into();
                }
            },
            Err(e) => {
                debug!(?e, "Unable to decode B64 BasicAuth");
                return empty_with_code(StatusCode::BAD_REQUEST).into();
            }
        };

        let Some((provided_username, provided_password)) = decoded.split_once(":") else {
            debug!("Unable to turn Basic auth into username & password");
            return empty_with_code(StatusCode::BAD_REQUEST).into();
        };

        let password_matches = match argon.verify_password(provided_password.as_bytes(), &password_hash) {
            Ok(()) => true,
            Err(Error::Password) => {
                false
            },
            Err(e) => {
                debug!(?e, "Error verifiying password");
                return empty_with_code(StatusCode::INTERNAL_SERVER_ERROR).into();
            }
        };

        if username != provided_username {
            debug!("Usernames didn't match for auth");
            return empty_with_code(StatusCode::UNAUTHORIZED).into();
        }

        if password_matches {
            AuthReturn::AuthConfirmed(req)
        } else {
            debug!("Passwords didn't match for auth");
            empty_with_code(StatusCode::UNAUTHORIZED).into()
        }
    }
}
