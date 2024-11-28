use std::collections::HashMap;
use std::ops::BitXor;
use hmac::Hmac;
use serde::Deserialize;
use serde_json::from_slice;
use sha2::digest::Mac;
use sha2::Sha256;
use crate::s3::get_bucket;

pub const AUTH_DATA_LOCATION: &str = "auth_data.json";

type HmacSha256 = Hmac<Sha256>;

fn hmac (key: &[u8], content: &[u8]) -> color_eyre::Result<Vec<u8>> {
    let mut hmac = HmacSha256::new_from_slice(key)?;
    hmac.update(content);
    Ok(hmac.finalize().into_bytes().to_vec())
}

#[derive(Deserialize)]
pub struct Auth {
    entries: HashMap<String, UsernameAndPassword>
}

#[derive(Deserialize)]
struct UsernameAndPassword {
    username: String,
    scrammed_password: String
}

impl Auth {
    pub async fn new () -> color_eyre::Result<Self> {
        let bucket = get_bucket();
        let file_contents = bucket.get_object(AUTH_DATA_LOCATION).await?.to_vec();

        Ok(from_slice(&file_contents)?)
    }

    pub fn protect (&mut self, pattern: String, username: String, password: String) -> color_eyre::Result<()> {
        fn Hi (key: &str, salt: &str, i: u32) -> color_eyre::Result<Vec<u8>> {
            let mut prev = hmac(key.as_bytes(), format!("{salt}1").as_bytes())?;
            let mut all: Vec<Vec<u8>> = vec![prev.clone()];
            for _ in 1..i {
                let next = hmac(key.as_bytes(), &prev)?;
                all.push(next.clone());
                prev = next;
            }

            Ok(all.into_iter().reduce(|acc, item| acc.into_iter().zip(item.into_iter()).map(|(a, b)| a.bitxor(b)).collect()).unwrap())
        }

        const I: u32 = 4096;
        let salt: &'static str = "FIXME"; //TODO: FIXME
        let salted_password: String = Hi(&password, salt, I)?.into_iter().map(|x| format!("{x:x}")).collect();

        self.entries.insert(pattern, UsernameAndPassword {
            username,
            scrammed_password: salted_password
        });

        Ok(())
    }
}