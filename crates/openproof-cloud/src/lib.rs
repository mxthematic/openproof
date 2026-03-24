use anyhow::{Context, Result};
use openproof_protocol::{
    CloudCorpusArtifactResponse, CloudCorpusAuthContext, CloudCorpusPackageSummary,
    CloudCorpusPackagesResponse, CloudCorpusSearchHit, CloudCorpusSearchResponse,
    CloudCorpusUploadBatchRequest, CloudCorpusUploadBatchResponse, CloudCorpusUploadItem,
    RemoteCorpusAvailability, ShareMode,
};
use reqwest::header::{HeaderMap, HeaderValue};

fn trim_text(value: &str) -> &str {
    value.trim()
}

fn normalize_base_url(value: &str) -> Option<String> {
    let trimmed = trim_text(value);
    if trimmed.is_empty() {
        None
    } else {
        // Strip trailing /api or /api/ to prevent doubled paths like /api/api/v1/...
        let cleaned = trimmed
            .trim_end_matches('/')
            .trim_end_matches("/api");
        Some(cleaned.to_string())
    }
}

fn parse_enabled_flag(value: &str) -> bool {
    let normalized = value.trim().to_lowercase();
    matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
}

/// Default production corpus URL -- bundled into the client.
/// Users don't need to configure anything; they just opt into community mode.
const DEFAULT_CORPUS_URL: &str = "https://openproof-cloud-production.up.railway.app";

/// Check remote corpus availability.
/// The corpus URL is bundled by default. Users can override with env vars
/// or disable with OPENPROOF_ENABLE_REMOTE_CORPUS=0.
pub fn get_remote_corpus_availability(
    base_url_override: Option<&str>,
    enable_flag_override: Option<&str>,
) -> RemoteCorpusAvailability {
    let enable_flag = enable_flag_override
        .map(String::from)
        .or_else(|| std::env::var("OPENPROOF_ENABLE_REMOTE_CORPUS").ok())
        .unwrap_or_else(|| "1".to_string()); // Enabled by default
    let enabled_by_flag = parse_enabled_flag(&enable_flag);

    let raw_url = base_url_override
        .map(String::from)
        .or_else(|| std::env::var("OPENPROOF_CORPUS_URL").ok())
        .unwrap_or_else(|| DEFAULT_CORPUS_URL.to_string());
    let base_url = normalize_base_url(&raw_url);

    if !enabled_by_flag {
        return RemoteCorpusAvailability {
            enabled_by_flag,
            base_url,
            available: false,
            reason: "disabled".to_string(),
        };
    }
    if base_url.is_none() {
        return RemoteCorpusAvailability {
            enabled_by_flag,
            base_url: None,
            available: false,
            reason: "missing_url".to_string(),
        };
    }
    RemoteCorpusAvailability {
        enabled_by_flag,
        base_url,
        available: true,
        reason: "ready".to_string(),
    }
}

fn default_dev_tenant_id() -> Option<String> {
    std::env::var("OPENPROOF_CORPUS_TENANT")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[derive(Debug, Clone, Default)]
pub struct CloudCorpusClientOptions {
    pub base_url: Option<String>,
    pub enable_flag: Option<String>,
}

/// HTTP client for the remote corpus API.
pub struct CloudCorpusClient {
    client: reqwest::Client,
    options: CloudCorpusClientOptions,
}

impl CloudCorpusClient {
    pub fn new(options: CloudCorpusClientOptions) -> Self {
        Self {
            client: reqwest::Client::new(),
            options,
        }
    }

    pub fn availability(&self) -> RemoteCorpusAvailability {
        get_remote_corpus_availability(
            self.options.base_url.as_deref(),
            self.options.enable_flag.as_deref(),
        )
    }

    pub fn base_url(&self) -> Option<String> {
        let avail = self.availability();
        if avail.available {
            avail.base_url
        } else {
            None
        }
    }

    pub fn is_configured(&self) -> bool {
        self.availability().available
    }

    pub fn describe(&self) -> String {
        match self.base_url() {
            Some(url) => format!("enabled at {url}"),
            None => "disabled".to_string(),
        }
    }

    fn resolve_dev_tenant_id(&self, auth: Option<&CloudCorpusAuthContext>) -> Option<String> {
        auth.and_then(|a| a.dev_tenant_id.clone())
            .filter(|v| !v.trim().is_empty())
            .or_else(default_dev_tenant_id)
    }

    fn build_headers(
        &self,
        auth: Option<&CloudCorpusAuthContext>,
        content_type: Option<&str>,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("accept", HeaderValue::from_static("application/json"));
        headers.insert(
            "x-openproof-client",
            HeaderValue::from_static("openproof-public"),
        );
        if let Some(ct) = content_type {
            if let Ok(val) = HeaderValue::from_str(ct) {
                headers.insert("content-type", val);
            }
        }
        if let Some(token) = auth.and_then(|a| a.bearer_token.as_deref()).filter(|t| !t.trim().is_empty()) {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {token}")) {
                headers.insert("authorization", val);
            }
        }
        if let Some(tenant) = self.resolve_dev_tenant_id(auth) {
            if let Ok(val) = HeaderValue::from_str(&tenant) {
                headers.insert("x-openproof-dev-tenant", val);
            }
        }
        headers
    }

    /// Generate a cache key for a given share mode and auth context.
    pub fn cache_key(
        &self,
        share_mode: ShareMode,
        auth: Option<&CloudCorpusAuthContext>,
    ) -> Option<String> {
        let base_url = self.base_url()?;
        if share_mode == ShareMode::Local {
            return None;
        }
        if share_mode == ShareMode::Private {
            let dev_tenant_id = self.resolve_dev_tenant_id(auth);
            return Some(match dev_tenant_id {
                Some(tid) => format!("{base_url}#{tid}"),
                None => format!("{base_url}#private"),
            });
        }
        Some(base_url)
    }

    /// Search the remote verified corpus.
    pub async fn search_verified_remote(
        &self,
        query: &str,
        limit: usize,
        share_mode: ShareMode,
        auth: Option<&CloudCorpusAuthContext>,
    ) -> Result<Vec<CloudCorpusSearchHit>> {
        let base_url = match self.base_url() {
            Some(url) => url,
            None => return Ok(Vec::new()),
        };
        if share_mode == ShareMode::Local {
            return Ok(Vec::new());
        }
        let clamped_limit = limit.max(1).min(32);
        let response = self
            .client
            .get(format!("{base_url}/api/v1/search"))
            .query(&[
                ("query", query),
                ("limit", &clamped_limit.to_string()),
            ])
            .headers(self.build_headers(auth, None))
            .send()
            .await
            .context("cloud corpus search request failed")?;
        if !response.status().is_success() {
            anyhow::bail!(
                "search request failed with status {}",
                response.status().as_u16()
            );
        }
        let payload: CloudCorpusSearchResponse = response
            .json()
            .await
            .context("parsing search response")?;
        Ok(payload.hits)
    }

    /// Upload a batch of verified items to the remote corpus.
    pub async fn upload_verified_batch(
        &self,
        visibility_scope: &str,
        items: Vec<CloudCorpusUploadItem>,
        auth: Option<&CloudCorpusAuthContext>,
    ) -> Result<CloudCorpusUploadBatchResponse> {
        let base_url = self
            .base_url()
            .context("cloud corpus URL is not configured")?;
        let payload = CloudCorpusUploadBatchRequest {
            visibility_scope: visibility_scope.to_string(),
            items,
        };
        let response = self
            .client
            .post(format!("{base_url}/api/v1/uploads/verified-batch"))
            .headers(self.build_headers(auth, Some("application/json")))
            .json(&payload)
            .send()
            .await
            .context("upload request failed")?;
        if !response.status().is_success() {
            anyhow::bail!(
                "upload request failed with status {}",
                response.status().as_u16()
            );
        }
        response
            .json()
            .await
            .context("parsing upload response")
    }

    /// Fetch a single artifact by ID.
    pub async fn fetch_artifact(
        &self,
        id: &str,
        auth: Option<&CloudCorpusAuthContext>,
    ) -> Result<Option<CloudCorpusArtifactResponse>> {
        let base_url = match self.base_url() {
            Some(url) => url,
            None => return Ok(None),
        };
        let response = self
            .client
            .get(format!(
                "{base_url}/api/v1/artifacts/{}",
                urlencoding::encode(id)
            ))
            .headers(self.build_headers(auth, None))
            .send()
            .await
            .context("artifact request failed")?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            anyhow::bail!(
                "artifact request failed with status {}",
                response.status().as_u16()
            );
        }
        let payload: CloudCorpusArtifactResponse = response
            .json()
            .await
            .context("parsing artifact response")?;
        Ok(Some(payload))
    }

    /// List available seed packages on the remote corpus.
    pub async fn list_seed_packages(
        &self,
        auth: Option<&CloudCorpusAuthContext>,
    ) -> Result<Vec<CloudCorpusPackageSummary>> {
        let base_url = match self.base_url() {
            Some(url) => url,
            None => return Ok(Vec::new()),
        };
        let response = self
            .client
            .get(format!("{base_url}/api/v1/packages"))
            .headers(self.build_headers(auth, None))
            .send()
            .await
            .context("packages request failed")?;
        if !response.status().is_success() {
            anyhow::bail!(
                "packages request failed with status {}",
                response.status().as_u16()
            );
        }
        let payload: CloudCorpusPackagesResponse = response
            .json()
            .await
            .context("parsing packages response")?;
        Ok(payload.packages)
    }

    /// Semantic search via the cloud's Qdrant-backed endpoint.
    /// Returns hits ranked by embedding similarity.
    pub async fn search_semantic(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SemanticSearchHit>> {
        let base_url = match self.base_url() {
            Some(url) => url,
            None => return Ok(Vec::new()),
        };
        let clamped_limit = limit.max(1).min(32);
        let response = match self
            .client
            .get(format!("{base_url}/api/v1/search/semantic"))
            .query(&[("query", query), ("limit", &clamped_limit.to_string())])
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(_) => {
                return Ok(Vec::new());
            }
        };
        if !response.status().is_success() {
            return Ok(Vec::new());
        }
        let payload: serde_json::Value = response.json().await.context("parsing semantic response")?;
        let hits = payload
            .get("hits")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|h| {
                        Some(SemanticSearchHit {
                            identity_key: h.get("identity_key")?.as_str()?.to_string(),
                            label: h.get("label")?.as_str()?.to_string(),
                            statement: h.get("statement").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            score: h.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(hits)
    }

    /// Upload a failed attempt to the cloud.
    pub async fn upload_failed_attempt(
        &self,
        attempt: serde_json::Value,
    ) -> Result<()> {
        let base_url = match self.base_url() {
            Some(url) => url,
            None => return Ok(()),
        };
        let response = self
            .client
            .post(format!("{base_url}/api/v1/uploads/failed-attempts"))
            .json(&serde_json::json!([attempt]))
            .send()
            .await
            .context("failed attempt upload request failed")?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            anyhow::bail!("failed attempt upload returned {status}: {detail}");
        }
        Ok(())
    }

    /// Search for failed attempts on the cloud corpus.
    pub async fn search_failures(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>> {
        let base_url = match self.base_url() {
            Some(url) => url,
            None => return Ok(Vec::new()),
        };
        let response = self
            .client
            .get(format!("{base_url}/api/v1/search/failures"))
            .query(&[("query", query), ("limit", &limit.to_string())])
            .send()
            .await
            .context("failure search request failed")?;
        if !response.status().is_success() {
            return Ok(Vec::new());
        }
        let payload: serde_json::Value = response.json().await?;
        Ok(payload
            .get("failures")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Upload corpus edges to cloud.
    pub async fn upload_corpus_edges(
        &self,
        edges: serde_json::Value,
    ) -> Result<()> {
        let base_url = match self.base_url() {
            Some(url) => url,
            None => return Ok(()),
        };
        let _ = self
            .client
            .post(format!("{base_url}/api/v1/uploads/edges"))
            .json(&edges)
            .send()
            .await
            .context("corpus edge upload failed")?;
        Ok(())
    }
}

/// A semantic search hit from the cloud corpus.
#[derive(Debug, Clone)]
pub struct SemanticSearchHit {
    pub identity_key: String,
    pub label: String,
    pub statement: String,
    pub score: f32,
}
