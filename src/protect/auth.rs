use crate::s3::get_bucket;
use color_eyre::eyre::bail;
use getrandom::getrandom;
use hmac::Hmac;
use serde::{Deserialize, Serialize};
use serde_json::from_slice;
use sha2::{digest::Mac, Sha256};
use std::{collections::HashMap, ops::BitXor};

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
    hmac.finalize().into_bytes().to_vec()
}

fn Hi(key: &str, salt: &mut [u8], i: u32) -> color_eyre::Result<Vec<u8>> {
    if salt.is_empty() {
        bail!("Salt cannot be empty");
    }

    salt[salt.len() - 1] = 1;
    let mut prev = hmac(key.as_bytes(), &salt)?;
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
                .zip(item.into_iter())
                .map(|(a, b)| a.bitxor(b))
                .collect()
        })
        .unwrap())
}

#[derive(Serialize, Deserialize)]
pub struct Auth {
    entries: HashMap<String, UsernameAndPassword>,
}

#[derive(Serialize, Deserialize)]
struct UsernameAndPassword {
    username: String,
    salt: [u8; 16],
    stored_key: Vec<u8>,
}

impl Auth {
    pub async fn new() -> color_eyre::Result<Self> {
        let bucket = get_bucket();
        let file_contents = bucket.get_object(AUTH_DATA_LOCATION).await?.to_vec();

        Ok(from_slice(&file_contents)?)
    }

    pub async fn save(self) -> color_eyre::Result<()> {
        let sered = serde_json::to_vec(&self)?;
        let bucket = get_bucket();
        bucket
            .put_object_with_content_type(AUTH_DATA_LOCATION, &sered, mime::JSON.as_str())
            .await?;

        Ok(())
    }

    pub fn protect(
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

        self.entries.insert(
            pattern,
            UsernameAndPassword {
                username,
                salt,
                stored_key,
            },
        );

        Ok(())
    }
}
