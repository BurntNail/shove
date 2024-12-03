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
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use hkdf::Hkdf;
use http_body_util::Full;
use hyper::{
    body::{Bytes, Incoming},
    http, Request, Response, StatusCode,
};
use s3::{error::S3Error, Bucket};
use serde::{Deserialize, Serialize};
use serde_json::{from_slice, from_value, to_vec, Value};
use sha2::Sha256;
use std::{
    collections::HashMap,
    env::var,
    net::{IpAddr, SocketAddr},
    num::NonZeroU32,
    sync::Arc,
};
use std::sync::LazyLock;
use tokio::sync::RwLock;
use uuid::Uuid;

pub const AUTH_DATA_LOCATION: &str = "authdata";

#[derive(Clone)]
pub struct AuthChecker {
    //deliberately not using dashmap as I want to be able to replace the entire map
    auth_users: Arc<RwLock<HashMap<Uuid, UsernameAndPassword>>>,
    auth_realms: Arc<RwLock<HashMap<String, Vec<Uuid>>>>,
    encryption_key: Key<Aes256Gcm>,
    rate_limiter: Arc<DefaultKeyedRateLimiter<IpAddr>>,
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

        let hk = Hkdf::<Sha256>::new(Some(salt.as_bytes()), password.as_bytes());
        let mut key_output = [0; 32];
        hk.expand(b"Auth Encryption Key", &mut key_output)?;

        let encryption_key = Key::<Aes256Gcm>::from_slice(&key_output).to_owned();

        let (realms, users) = Self::read_from_s3(bucket, &encryption_key).await?;

        let rate_limiter = Arc::new(RateLimiter::keyed(Quota::per_minute(
            NonZeroU32::new(10).unwrap(),
        )));

        Ok(Self {
            auth_realms: Arc::new(RwLock::new(realms)),
            auth_users: Arc::new(RwLock::new(users)),
            encryption_key,
            rate_limiter,
        })
    }

    async fn read_from_s3(
        bucket: &Bucket,
        key: &Key<Aes256Gcm>,
    ) -> color_eyre::Result<(HashMap<String, Vec<Uuid>>, HashMap<Uuid, UsernameAndPassword>)> {
        let contents = match bucket.get_object(AUTH_DATA_LOCATION).await {
            Ok(x) => x.to_vec(),
            Err(S3Error::HttpFailWithBody(404, _)) => return Ok((HashMap::new(), HashMap::new())),
            Err(e) => return Err(e.into()),
        };

        let (nonce, ciphered_data) = contents.split_at(12);
        let nonce = Nonce::<Aes256Gcm>::from_slice(nonce);
        let cipher = Aes256Gcm::new(key);
        let json = cipher.decrypt(nonce, ciphered_data)?;
        let value: Value = from_slice(&json)?;

        let Some(realms) = value.get("realms").cloned() else {
            return Ok((HashMap::new(), HashMap::new()));
        };
        let Some(users) = value.get("users").cloned() else {
            return Ok((HashMap::new(), HashMap::new()));
        };

        Ok((from_value(realms)?, from_value(users)?))
    }

    pub async fn save_to_s3(self, bucket: &Bucket) -> color_eyre::Result<()> {
        let mut nonce_data = [0; 12];
        getrandom(&mut nonce_data)?;
        let nonce = Nonce::<Aes256Gcm>::from_slice(&nonce_data);

        let sered = {
            let auth_users = self.auth_users.read().await;
            let auth_realms = self.auth_realms.read().await;

            let auth_users = auth_users.clone();
            let auth_realms = auth_realms.clone();

            let obj = serde_json::json!({
                "realms": auth_realms,
                "users": auth_users
            });
            to_vec(&obj)?
        };

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

    pub async fn get_patterns_and_usernames(&self) -> Vec<(String, Vec<String>)> {
        let users = self.auth_users.read().await;
        self.auth_realms.read().await.clone()
            .into_iter()
            .map(|(pat, uuids)| {
                (
                    pat,
                    uuids
                        .into_iter()
                        .flat_map(|uuid| users.get(&uuid))
                        .map(|x| x.username.clone())
                        .collect()
                    )
            })
            .collect()
    }

    pub async fn get_users (&self) -> Vec<(Uuid, String)> {
        self.auth_users.read().await.clone().into_iter().map(|(uuid, uap)| (uuid, uap.username)).collect()
    }

    pub async fn rm_pattern(&self, pattern: &str) {
        self.auth_realms.write().await.remove(pattern);
    }

    pub async fn rm_user(&self, user: &Uuid) {
        let mut realms = self.auth_realms.write().await;
        for (_, list) in realms.iter_mut() {
            list.retain_mut(|uuid| uuid != user);
        }
        self.auth_users.write().await.remove(user);
    }

    pub async fn get_all_realms (&self) -> Vec<String> {
        self.auth_realms.read().await.iter().map(|(pat, _)| pat).cloned().collect()
    }

    pub async fn reload(&self, bucket: &Bucket) -> color_eyre::Result<()> {
        let (realms, users) = Self::read_from_s3(bucket, &self.encryption_key).await?;
        *self.auth_realms.write().await = realms;
        *self.auth_users.write().await = users;
        Ok(())
    }

    pub async fn add_user (&self, username: String, password: impl AsRef<[u8]>) -> color_eyre::Result<Uuid> {
        let mut salt = [0; 32];
        getrandom(&mut salt)?;
        let saltstring = SaltString::encode_b64(&salt)?;

        let argon2 = Argon2::default();
        let password_hash = argon2.hash_password(password.as_ref(), &saltstring)?;
        let stored_key = password_hash.serialize().to_string();

        let uuid = Uuid::now_v7();

        self.auth_users.write().await.insert(uuid, UsernameAndPassword {
            username: username.to_string(),
            stored_key
        });

        Ok(uuid)
    }

    pub async fn protect(
        &self,
        pattern: String,
        uuids: Vec<Uuid>,
    ) {
        let mut realms = self.auth_realms.write().await;

        *realms.entry(pattern)
            .or_default() = uuids;
    }

    pub async fn protect_additional(
        &self,
        pattern: String,
        uuids: Vec<Uuid>,
    ) {
        let mut realms = self.auth_realms.write().await;

        realms.entry(pattern)
            .or_default()
            .extend(uuids);
    }

    pub async fn get_users_with_access_to_realm (&self, pat: &str) -> Vec<Uuid> {
        self.auth_realms.read().await
            .iter()
            .find(|(this_pat, _)| this_pat == &pat)
            .map(|(_, uuids)| uuids)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn check_auth(
        &self,
        path: &str,
        req: Request<Incoming>,
        remote_addr: SocketAddr,
    ) -> AuthReturn {
        static FAKE_PASSWORD: LazyLock<String> = LazyLock::new(|| {
            const FAKE_PASSWORD_ACTUAL: &str = "thisismyfakepasswordtoreducesidechannelattackswhereyoumightbeabletoworkoutwhetheryourusernamewasanactualusernameforthisrealm";
            let mut salt = [0; 32];
            getrandom(&mut salt).expect("unable to get salt for fake password");
            let saltstring = SaltString::encode_b64(&salt).expect("unable to encode salt for fake password");

            let hashed = Argon2::default().hash_password(FAKE_PASSWORD_ACTUAL.as_bytes(), &saltstring).expect("unable to hash fake password");
            hashed.serialize().to_string()
        });

        let users: Vec<UsernameAndPassword> = {
            let realms = self.auth_realms.read().await;
            let users = self.auth_users.read().await;
            let Some(uuids) = realms
                .iter()
                .find(|(pattern, _)| path.starts_with(pattern.as_str()))
                .map(|(_, uap)| uap.clone())
            else {
                return AuthReturn::AuthConfirmed(req);
            };

            uuids.into_iter().filter_map(|uuid| users.get(&uuid)).cloned().collect()
        };

        let ip = remote_addr.ip();
        if self.rate_limiter.check_key(&ip).is_err() {
            return empty_with_code(StatusCode::TOO_MANY_REQUESTS).into();
        }

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

        //technically, usernames can have colons so we do this
        let Some(colon_index) = decoded.rfind(':') else {
            debug!("Unable to turn Basic auth into username & password");
            return empty_with_code(StatusCode::BAD_REQUEST).into();
        };
        let (provided_username, provided_password) = decoded.split_at(colon_index);
        let provided_password = &provided_password[1..];

        let Some(UsernameAndPassword {username: _, stored_key}) = users.into_iter().find(|x| x.username == provided_username) else {
            debug!("Usernames didn't match for auth");
            let fake_password_hash = match PasswordHash::new(&FAKE_PASSWORD) {
                Ok(x) => x,
                Err(e) => {
                    error!(?e, "Unable to decode stored fake password");
                    return empty_with_code(StatusCode::INTERNAL_SERVER_ERROR).into();
                }
            };
            let _ = Argon2::default().verify_password(provided_password.as_bytes(), &fake_password_hash);
            return empty_with_code(StatusCode::UNAUTHORIZED).into();
        };

        let password_hash = match PasswordHash::new(&stored_key) {
            Ok(x) => x,
            Err(e) => {
                error!(?e, "Unable to decode stored password key");
                return empty_with_code(StatusCode::INTERNAL_SERVER_ERROR).into();
            }
        };

        let password_matches =
            match Argon2::default().verify_password(provided_password.as_bytes(), &password_hash) {
                Ok(()) => true,
                Err(Error::Password) => false,
                Err(e) => {
                    debug!(?e, "Error verifying password");
                    return empty_with_code(StatusCode::INTERNAL_SERVER_ERROR).into();
                }
            };


        if password_matches {
            AuthReturn::AuthConfirmed(req)
        } else {
            debug!("Passwords didn't match for auth");
            empty_with_code(StatusCode::UNAUTHORIZED).into()
        }
    }
}
