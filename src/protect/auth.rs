use crate::serve::empty_with_code;
use aes_gcm::{
    aead::{Aead, Nonce},
    Aes256Gcm, Key, KeyInit,
};
use argon2::{
    password_hash::{Error, SaltString},
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
};
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
use std::{collections::HashMap, env::var, sync::Arc};
use tokio::sync::RwLock;

pub const AUTH_DATA_LOCATION: &str = "authdata";

#[derive(Clone)]
pub struct AuthChecker {
    //deliberately not using dashmap as I want to be able to replace the entire map
    entries: Arc<RwLock<HashMap<String, UsernameAndPassword>>>,
    encryption_key: Key<Aes256Gcm>,
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
        let password =
            var("AUTH_ENCRYPTION_KEY").expect("unable to find env var AUTH_ENCRYPTION_KEY");
        let salt = &bucket.name;
        let mut key_output = [0; 32];
        Argon2::default().hash_password_into(
            password.as_bytes(),
            salt.as_bytes(),
            &mut key_output,
        )?;

        let encryption_key = Key::<Aes256Gcm>::from_slice(&key_output).to_owned();

        let entries = Arc::new(RwLock::new(
            Self::read_from_s3(bucket, &encryption_key).await?,
        ));

        Ok(Self {
            entries,
            encryption_key,
        })
    }

    async fn read_from_s3(
        bucket: &Bucket,
        key: &Key<Aes256Gcm>,
    ) -> color_eyre::Result<HashMap<String, UsernameAndPassword>> {
        let contents = match bucket.get_object(AUTH_DATA_LOCATION).await {
            Ok(x) => x.to_vec(),
            Err(S3Error::HttpFailWithBody(404, _)) => return Ok(HashMap::new()),
            Err(e) => return Err(e.into()),
        };

        let (nonce, ciphered_data) = contents.split_at(12);
        let nonce = Nonce::<Aes256Gcm>::from_slice(nonce);
        let cipher = Aes256Gcm::new(key);

        let json = cipher.decrypt(nonce, ciphered_data)?;
        Ok(from_slice(&json)?)
    }

    pub async fn save_to_s3(self, bucket: &Bucket) -> color_eyre::Result<()> {
        let mut nonce_data = [0; 12];
        getrandom(&mut nonce_data)?;
        let nonce = Nonce::<Aes256Gcm>::from_slice(&nonce_data);

        let readable = self.entries.read().await;
        let sered = serde_json::to_vec(&*readable)?;

        let cipher = Aes256Gcm::new(&self.encryption_key);
        let ciphered_data = cipher.encrypt(nonce, sered.as_slice())?;

        let mut encrypted_data = nonce_data.to_vec();
        encrypted_data.extend(ciphered_data);

        bucket
            .put_object_with_content_type(
                AUTH_DATA_LOCATION,
                &encrypted_data,
                "application/octet-stream",
            )
            .await?;

        Ok(())
    }

    pub async fn rm_pattern(&self, pattern: &str) {
        let mut entries = self.entries.write().await;
        entries.remove(pattern);
    }

    pub async fn get_patterns_and_usernames(&self) -> Vec<(String, String)> {
        self.entries
            .read()
            .await
            .iter()
            .map(|(pat, uap)| (pat.clone(), uap.username.clone()))
            .collect()
    }

    pub async fn reload(&self, bucket: &Bucket) -> color_eyre::Result<()> {
        let entries = Self::read_from_s3(bucket, &self.encryption_key).await?;
        *self.entries.write().await = entries;
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

        let password_matches =
            match argon.verify_password(provided_password.as_bytes(), &password_hash) {
                Ok(()) => true,
                Err(Error::Password) => false,
                Err(e) => {
                    debug!(?e, "Error verifying password");
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
