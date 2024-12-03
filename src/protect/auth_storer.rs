use aes_gcm::{
    aead::{Aead, Nonce},
    Aes256Gcm, Key, KeyInit,
};
use argon2::{password_hash::SaltString, Argon2, PasswordHasher};
use getrandom::getrandom;
use hkdf::Hkdf;
use s3::{error::S3Error, Bucket};
use serde::{Deserialize, Serialize};
use serde_json::{from_slice, to_vec};
use sha2::Sha256;
use std::{collections::HashMap, env::var, sync::LazyLock};
use uuid::Uuid;

static AUTH_KEY: LazyLock<Key<Aes256Gcm>> = LazyLock::new(|| {
    let password = var("AUTH_ENCRYPTION_KEY").expect("unable to find env var AUTH_ENCRYPTION_KEY");
    let salt = var("BUCKET_NAME").expect("unable to find env var BUCKET_NAME");

    let hk = Hkdf::<Sha256>::new(Some(salt.as_bytes()), password.as_bytes());
    let mut key_output = [0; 32];
    hk.expand(b"Auth Encryption Key", &mut key_output)
        .expect("unable to expand key");

    Key::<Aes256Gcm>::from_slice(&key_output).to_owned()
});

pub const AUTH_DATA_LOCATION: &str = "authdata";

#[derive(Serialize, Deserialize, Clone, Hash, Eq, PartialEq, Debug)]
pub enum Realm {
    StartsWith(String),
}

impl Realm {
    pub fn matches(&self, path: &str) -> bool {
        match self {
            Self::StartsWith(pattern) => path.starts_with(pattern),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct UsernameAndPassword {
    pub username: String,
    pub stored_key: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AuthStorer {
    realms: HashMap<Realm, Vec<Uuid>>,
    users: HashMap<Uuid, UsernameAndPassword>,
}

impl AuthStorer {
    pub async fn new(bucket: &Bucket) -> color_eyre::Result<Self> {
        let contents = match bucket.get_object(AUTH_DATA_LOCATION).await {
            Ok(x) => x.to_vec(),
            Err(S3Error::HttpFailWithBody(404, _)) => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };

        let (nonce, ciphered_data) = contents.split_at(12);
        let nonce = Nonce::<Aes256Gcm>::from_slice(nonce);
        let cipher = Aes256Gcm::new(&*AUTH_KEY);
        let json = cipher.decrypt(nonce, ciphered_data)?;

        Ok(from_slice(&json)?)
    }

    pub async fn save(&self, bucket: &Bucket) -> color_eyre::Result<()> {
        let mut nonce_data = [0; 12];
        getrandom(&mut nonce_data)?;
        let nonce = Nonce::<Aes256Gcm>::from_slice(&nonce_data);

        let json = to_vec(&self)?;

        let cipher = Aes256Gcm::new(&*AUTH_KEY);
        let ciphered_data = cipher.encrypt(nonce, json.as_slice())?;

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

    pub fn get_patterns_and_usernames(&self) -> Vec<(Realm, Vec<String>)> {
        self.realms
            .iter()
            .map(|(pat, uuids)| {
                (
                    pat.clone(),
                    uuids
                        .iter()
                        .flat_map(|uuid| self.users.get(uuid))
                        .map(|x| x.username.clone())
                        .collect(),
                )
            })
            .collect()
    }

    pub fn get_users(&self) -> Vec<(Uuid, String)> {
        self.users
            .clone()
            .into_iter()
            .map(|(uuid, uap)| (uuid, uap.username))
            .collect()
    }

    pub fn rm_realm(&mut self, realm: &Realm) {
        self.realms.remove(realm);
    }

    pub fn rm_user(&mut self, user: &Uuid) {
        for (_, list) in self.realms.iter_mut() {
            list.retain_mut(|uuid| uuid != user);
        }
        self.users.remove(user);
    }

    pub fn get_all_realms(&self) -> Vec<Realm> {
        self.realms.keys().cloned().collect()
    }

    pub fn add_user(
        &mut self,
        username: String,
        password: impl AsRef<[u8]>,
    ) -> color_eyre::Result<Uuid> {
        let mut salt = [0; 32];
        getrandom(&mut salt)?;
        let saltstring = SaltString::encode_b64(&salt)?;

        let argon2 = Argon2::default();
        let password_hash = argon2.hash_password(password.as_ref(), &saltstring)?;
        let stored_key = password_hash.serialize().to_string();

        let uuid = Uuid::now_v7();

        self.users.insert(
            uuid,
            UsernameAndPassword {
                username: username.to_string(),
                stored_key,
            },
        );

        Ok(uuid)
    }

    pub fn protect(&mut self, pattern: Realm, uuids: Vec<Uuid>) {
        *self.realms.entry(pattern).or_default() = uuids;
    }

    pub fn protect_additional(&mut self, pattern: Realm, uuids: Vec<Uuid>) {
        self.realms.entry(pattern).or_default().extend(uuids);
    }

    pub fn get_users_with_access_to_realm(&self, pat: &Realm) -> Vec<Uuid> {
        self.realms
            .iter()
            .find(|(this_pat, _)| this_pat == &pat)
            .map(|(_, uuids)| uuids)
            .cloned()
            .unwrap_or_default()
    }

    ///None signifies everyone (even unauth) has access
    pub fn find_users_with_access(&self, path: &str) -> Option<HashMap<String, String>> {
        let uuids = self
            .realms
            .iter()
            .find(|(pattern, _)| pattern.matches(path))
            .map(|(_, uap)| uap.clone())?;

        Some(
            uuids
                .into_iter()
                .filter_map(|uuid| self.users.get(&uuid))
                .cloned()
                .map(|uap| (uap.username, uap.stored_key))
                .collect(),
        )
    }
}
