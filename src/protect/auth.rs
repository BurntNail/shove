use color_eyre::eyre::bail;
use getrandom::getrandom;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::from_slice;
use sha2::{Sha256, Digest};
use std::{collections::HashMap, ops::BitXor};
use std::sync::Arc;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use s3::Bucket;
use tokio::sync::RwLock;

pub const AUTH_DATA_LOCATION: &str = "auth_data.json";

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
fn Hi(key: &str, salt: &mut [u8], i: u32) -> color_eyre::Result<Vec<u8>> {
    if salt.is_empty() {
        bail!("Salt cannot be empty");
    }

    salt[salt.len() - 1] = 1;
    let mut prev = hmac(key.as_bytes(), salt)?;
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
    //deliberately not using dashmap as i want to be able to replace the entire map
    entries: Arc<RwLock<HashMap<String, UsernameAndPassword>>>,
}

#[derive(Serialize, Deserialize)]
struct UsernameAndPassword {
    username: String,
    salt: [u8; 16],
    stored_key: Vec<u8>,
}

impl AuthChecker {
    pub async fn new(bucket: &Bucket) -> color_eyre::Result<Self> {
        let file_contents = bucket.get_object(AUTH_DATA_LOCATION).await?.to_vec();
        let entries = Arc::new(RwLock::new(from_slice(&file_contents)?));


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
        const I: u32 = 4096;
        let mut salt = [0; 16];
        getrandom(&mut salt)?;

        let salted_password = Hi(&password, &mut salt, I)?;
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

    pub async fn check_auth (&self, path: &str, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Request<Incoming>> {
        let readable = self.entries.read().await;
        let Some((_, UsernameAndPassword {
            username, salt, stored_key
        })) = readable.iter().find(|(pattern, _)| path.contains(pattern.as_str())).cloned() else {
            return Err(req);
        };

        todo!()
    }
}
