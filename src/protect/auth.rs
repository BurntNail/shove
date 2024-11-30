use color_eyre::eyre::bail;
use getrandom::getrandom;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::from_slice;
use sha2::{Sha256, Digest};
use std::{collections::HashMap, ops::BitXor};
use std::sync::Arc;
use base64::{Engine};
use base64::prelude::BASE64_STANDARD;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{http, Request, Response, StatusCode};
use s3::Bucket;
use s3::error::S3Error;
use tokio::sync::RwLock;
use crate::serve::empty_with_code;

pub const AUTH_DATA_LOCATION: &str = "auth_data.json";
const I: u32 = 4096;

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8], content: &[u8]) -> color_eyre::Result<Vec<u8>> {
    let mut hmac = HmacSha256::new_from_slice(key)?;
    hmac.update(content);
    Ok(hmac.finalize().into_bytes().to_vec())
}

fn h (content: &[u8]) -> Vec<u8> {
    let mut sha = Sha256::default();
    sha.update(content);
    sha.finalize().to_vec()
}

#[allow(non_snake_case)]
fn Hi(key: &str, salt: &[u8], i: u32) -> color_eyre::Result<Vec<u8>> {
    if salt.is_empty() {
        bail!("Salt cannot be empty");
    }

    let mut salt = salt.to_vec();
    let mut prev = hmac(key.as_bytes(), &mut salt)?;
    let mut all: Vec<Vec<u8>> = vec![prev.clone()];
    for _ in 1..i {
        let next = hmac(key.as_bytes(), &prev)?;
        all.push(next.clone());
        prev = next;
    }

    Ok(all
        .into_iter()
        .reduce(|acc, item| {
            acc.into_iter()
                .zip(item)
                .map(|(a, b)| a.bitxor(b))
                .collect()
        })
        .unwrap())
}

#[derive(Clone)]
pub struct AuthChecker {
    //deliberately not using dashmap as I want to be able to replace the entire map
    entries: Arc<RwLock<HashMap<String, UsernameAndPassword>>>,
}

pub enum AuthReturn {
    AuthConfirmed(Request<Incoming>),
    ResponseFromAuth(Response<Full<Bytes>>),
    Error(http::Error)
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
    salt: [u8; 16],
    stored_key: Vec<u8>,
}

impl AuthChecker {
    pub async fn new(bucket: &Bucket) -> color_eyre::Result<Self> {
        let entries = match bucket.get_object(AUTH_DATA_LOCATION).await {
            Ok(x) => from_slice(&x.to_vec()).ok(),
            Err(S3Error::HttpFailWithBody(404, _)) => None,
            Err(e) => return Err(e.into()),
        };

        let entries = Arc::new(RwLock::new(entries.unwrap_or_default()));


        Ok(Self {
            entries
        })
    }

    pub async fn reload (&self, bucket: &Bucket) -> color_eyre::Result<()> {
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
        let mut salt = [0; 16];
        getrandom(&mut salt)?;

        let salted_password = Hi(&password, &salt, I)?;
        let client_key = hmac(&salted_password, b"Client Key")?;
        let stored_key = h(&client_key);

        let mut writeable = self.entries.write().await;

        writeable.insert(
            pattern,
            UsernameAndPassword {
                username,
                salt,
                stored_key,
            },
        );

        Ok(())
    }

    pub async fn check_auth (&self, path: &str, req: Request<Incoming>) -> AuthReturn {
        let readable = self.entries.read().await;
        let Some(UsernameAndPassword {
                     username, salt, stored_key
                 }) = readable.iter().find(|(pattern, _)| path.contains(pattern.as_str())).map(|(_, uap)| uap.clone()) else {
            return AuthReturn::AuthConfirmed(req);
        };

        let headers = req.headers();
        let provided_auth_b64 = match headers.get("Authorization").cloned() {
            Some(x) => match x.to_str() {
                Ok(x) => match x.strip_prefix("Basic ") {
                    Some(x) => x.to_string(),
                    None => {
                        warn!("Unable to find Basic part");
                        return empty_with_code(StatusCode::UNAUTHORIZED).into();
                    }
                },
                Err(e) => {
                    warn!(?e, "Error converting auth part to string");
                    return empty_with_code(StatusCode::BAD_REQUEST).into();
                }
            },
            None => {
                return Response::builder()
                    .header("WWW-Authenticate", format!("Basic realm={path:?} charset=\"UTF-8\""))
                    .status(StatusCode::UNAUTHORIZED)
                    .body(Full::default()).into();
            },
        };

        let decoded = match BASE64_STANDARD.decode(&provided_auth_b64) {
            Ok(dec) => match String::from_utf8(dec) {
                Ok(dec) => dec,
                Err(e) => {
                    warn!(?e, "Unable to turn decoded B64 BasicAuth into string");
                    return empty_with_code(StatusCode::BAD_REQUEST).into();
                }
            }
            Err(e) => {
                warn!(?e, "Unable to decode B64 BasicAuth");
                return empty_with_code(StatusCode::BAD_REQUEST).into();
            }
        };

        let Some((provided_username, provided_password)) = decoded.split_once(":") else {
            warn!("Unable to turn Basic auth into username & password");
            return empty_with_code(StatusCode::BAD_REQUEST).into();
        };

        if username != provided_username {
            warn!("Usernames didn't match for auth");
            return empty_with_code(StatusCode::UNAUTHORIZED).into();
        }

        let salted_password = match Hi(&provided_password, &salt, I) {
            Ok(x) => x,
            Err(e) => {
                error!(?e, "Error hashing provided password");
                return empty_with_code(StatusCode::INTERNAL_SERVER_ERROR).into();
            }
        };
        let client_key = match hmac(&salted_password, b"Client Key") {
            Ok(x) => x,
            Err(e) => {
                error!(?e, "Error hashing client key");
                return empty_with_code(StatusCode::INTERNAL_SERVER_ERROR).into();
            }
        };
        let provided_stored_key = h(&client_key);


        if provided_stored_key != stored_key {
            warn!("Passwords didn't match for auth");
            empty_with_code(StatusCode::UNAUTHORIZED).into()
        } else {
            AuthReturn::AuthConfirmed(req)
        }
    }
}
