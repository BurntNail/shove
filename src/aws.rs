use crate::UploadData;
use s3::{creds::Credentials, error::S3Error, Bucket, Region};
use std::env;

pub const UPLOAD_DATA_LOCATION: &str = "upload_data.json";

pub fn get_bucket() -> Box<Bucket> {
    let aws_creds = get_aws_creds();
    let bucket_name = env::var("BUCKET_NAME").expect("expected env var BUCKET_NAME");
    let endpoint = env::var("AWS_ENDPOINT_URL_S3").expect("expected env var AWS_ENDPOINT_URL_S3");
    let region = Region::Custom {
        region: "auto".to_owned(),
        endpoint,
    };
    let bucket = Bucket::new(&bucket_name, region, aws_creds).unwrap();
    bucket
}

pub fn get_aws_creds() -> Credentials {
    let access_key = env::var("AWS_ACCESS_KEY_ID").expect("expected env var AWS_ACCESS_KEY_ID");
    let secret_key =
        env::var("AWS_SECRET_ACCESS_KEY").expect("expected env var AWS_SECRET_ACCESS_KEY");

    Credentials::new(Some(&access_key), Some(&secret_key), None, None, None).unwrap()
}

pub async fn get_upload_data(bucket: &Box<Bucket>) -> color_eyre::Result<Option<UploadData>> {
    let data = bucket.get_object(UPLOAD_DATA_LOCATION).await;
    match data {
        Ok(data) => Ok(Some(serde_json::from_slice(data.as_slice())?)),
        Err(e) => match e {
            S3Error::HttpFailWithBody(404, _) => Ok(None),
            _ => Err(e.into()),
        },
    }
}
