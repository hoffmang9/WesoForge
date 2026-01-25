use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use reqwest::Url;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub(crate) enum BackendError {
    #[error("invalid reward address")]
    InvalidRewardAddress,
}

#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    code: String,
}

#[derive(Debug, Serialize)]
struct WorkRequest {
    count: u32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BackendWorkBatch {
    pub(crate) lease_id: String,
    pub(crate) lease_expires_at: i64,
    pub(crate) jobs: Vec<BackendJobDto>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct BackendJobDto {
    pub(crate) job_id: u64,
    pub(crate) height: u32,
    pub(crate) field_vdf: i32,
    pub(crate) challenge_b64: String,
    pub(crate) number_of_iterations: u64,
    pub(crate) output_b64: String,
}

#[derive(Debug, Serialize)]
struct SubmitRequest {
    lease_id: String,
    witness_b64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reward_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SubmitResponse {
    pub(crate) reason: String,
    pub(crate) detail: String,
    pub(crate) accepted_event_id: Option<u64>,
}

pub(crate) async fn fetch_work(
    http: &reqwest::Client,
    backend: &Url,
    count: u32,
) -> anyhow::Result<BackendWorkBatch> {
    let url = backend.join("api/jobs/lease_proofs")?;
    let res = http
        .post(url)
        .json(&WorkRequest { count })
        .send()
        .await?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::BAD_REQUEST {
            if let Ok(err) = serde_json::from_str::<ApiErrorBody>(&body) {
                if err.code == "invalid_reward_address" {
                    return Err(BackendError::InvalidRewardAddress.into());
                }
            }
        }
        anyhow::bail!("http {status}: {body}");
    }
    Ok(res.json().await?)
}

pub(crate) async fn submit_job(
    http: &reqwest::Client,
    backend: &Url,
    job_id: u64,
    lease_id: &str,
    witness: &[u8],
    reward_address: Option<&str>,
    name: Option<&str>,
) -> anyhow::Result<SubmitResponse> {
    let url = backend.join(&format!("api/jobs/{job_id}/submit"))?;
    let res = http
        .post(url)
        .json(&SubmitRequest {
            lease_id: lease_id.to_string(),
            witness_b64: B64.encode(witness),
            reward_address: reward_address.map(str::to_string),
            name: name.map(str::to_string),
        })
        .send()
        .await?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::BAD_REQUEST {
            if let Ok(err) = serde_json::from_str::<ApiErrorBody>(&body) {
                if err.code == "invalid_reward_address" {
                    return Err(BackendError::InvalidRewardAddress.into());
                }
            }
        }
        anyhow::bail!("http {status}: {body}");
    }
    Ok(res.json().await?)
}
