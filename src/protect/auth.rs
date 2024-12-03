use crate::{
    hash_raw_bytes,
    protect::auth_storer::{AuthStorer, Realm},
    serve::empty_with_code,
};
use argon2::{
    password_hash::{Error, SaltString},
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
};
use base64::{prelude::BASE64_STANDARD, Engine};
use getrandom::getrandom;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use http_body_util::Full;
use hyper::{
    body::{Bytes, Incoming},
    http, Request, Response, StatusCode,
};
use s3::Bucket;
use std::{
    net::{IpAddr, SocketAddr},
    num::NonZeroU32,
    sync::{Arc, LazyLock},
};
use color_eyre::eyre::bail;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

pub const AUTH_DATA_LOCATION: &str = "authdata";

#[derive(Clone)]
pub struct AuthChecker {
    auth: Arc<RwLock<AuthStorer>>,
    last_hash: Arc<Mutex<Vec<u8>>>,
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

impl AuthChecker {
    pub async fn new(bucket: &Bucket) -> color_eyre::Result<Self> {
        let (auth_storer, raw_bytes) = AuthStorer::new(bucket).await?;
        let hashed_bytes = hash_raw_bytes(&raw_bytes);

        let rate_limiter = Arc::new(RateLimiter::keyed(Quota::per_minute(
            NonZeroU32::new(10).unwrap(),
        )));

        Ok(Self {
            auth: Arc::new(RwLock::new(auth_storer)),
            last_hash: Arc::new(Mutex::new(hashed_bytes)),
            rate_limiter,
        })
    }

    pub async fn check_and_reload(&self, bucket: &Bucket) -> color_eyre::Result<()> {
        let Ok(mut last_hash) = self.last_hash.try_lock() else {
            bail!("already reloading auth")
        };

        let current_enc_bytes = AuthStorer::get_encrypted_bytes(bucket).await?;
        let hashed = hash_raw_bytes(&current_enc_bytes);

        if *last_hash != hashed {
            *last_hash = hashed;

            let new_version = AuthStorer::construct_from_enc_bytes(&current_enc_bytes)?;
            *self.auth.write().await = new_version;
        }
        Ok(())
    }

    pub async fn save_to_s3(&self, bucket: &Bucket) -> color_eyre::Result<()> {
        self.auth.read().await.save(bucket).await
    }

    pub async fn get_patterns_and_usernames(&self) -> Vec<(Realm, Vec<String>)> {
        self.auth.read().await.get_patterns_and_usernames()
    }

    pub async fn get_users(&self) -> Vec<(Uuid, String)> {
        self.auth.read().await.get_users()
    }

    pub async fn rm_realm(&self, realm: &Realm) {
        self.auth.write().await.rm_realm(realm);
    }

    pub async fn rm_user(&self, user: &Uuid) {
        self.auth.write().await.rm_user(user);
    }

    pub async fn get_all_realms(&self) -> Vec<Realm> {
        self.auth.read().await.get_all_realms()
    }

    pub async fn add_user(
        &self,
        username: String,
        password: impl AsRef<[u8]>,
    ) -> color_eyre::Result<Uuid> {
        self.auth.write().await.add_user(username, password)
    }

    pub async fn protect(&self, pattern: Realm, uuids: Vec<Uuid>) {
        self.auth.write().await.protect(pattern, uuids);
    }

    pub async fn protect_additional(&self, pattern: Realm, uuids: Vec<Uuid>) {
        self.auth.write().await.protect_additional(pattern, uuids);
    }

    pub async fn get_users_with_access_to_realm(&self, pat: &Realm) -> Vec<Uuid> {
        self.auth.read().await.get_users_with_access_to_realm(pat)
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
            let saltstring =
                SaltString::encode_b64(&salt).expect("unable to encode salt for fake password");

            let hashed = Argon2::default()
                .hash_password(FAKE_PASSWORD_ACTUAL.as_bytes(), &saltstring)
                .expect("unable to hash fake password");
            hashed.serialize().to_string()
        });

        let Some(users) = self.auth.read().await.find_users_with_access(path) else {
            return AuthReturn::AuthConfirmed(req);
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

        let Some(stored_key) = users.get(provided_username) else {
            debug!("Usernames didn't match for auth");
            let fake_password_hash = match PasswordHash::new(&FAKE_PASSWORD) {
                Ok(x) => x,
                Err(e) => {
                    error!(?e, "Unable to decode stored fake password");
                    return empty_with_code(StatusCode::INTERNAL_SERVER_ERROR).into();
                }
            };
            let _ = Argon2::default()
                .verify_password(provided_password.as_bytes(), &fake_password_hash);
            return empty_with_code(StatusCode::UNAUTHORIZED).into();
        };

        let password_hash = match PasswordHash::new(stored_key) {
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
