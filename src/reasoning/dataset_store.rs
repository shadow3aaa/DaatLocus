use miette::{Result, miette};
use serde::de::DeserializeOwned;

pub fn decode_dataset_json<T: DeserializeOwned>(dataset_name: &str, json: &str) -> Result<T> {
    serde_json::from_str::<T>(json)
        .map_err(|err| miette!("failed to decode embedded dataset {dataset_name}: {err}"))
}
