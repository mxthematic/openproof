//! Vector embeddings for semantic corpus search.
//!
//! Uses OpenAI text-embedding-3-small for embedding generation and
//! Qdrant for vector storage and similarity search.

use anyhow::Result;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, PointStruct, SearchPointsBuilder, UpsertPointsBuilder,
    VectorParamsBuilder,
};
use qdrant_client::Qdrant;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const COLLECTION_NAME: &str = "verified_corpus";
const EMBEDDING_DIM: u64 = 1536; // text-embedding-3-small
const EMBEDDING_MODEL: &str = "text-embedding-3-small";
const OPENAI_EMBEDDING_URL: &str = "https://api.openai.com/v1/embeddings";

/// A semantic search result from the vector store.
#[derive(Debug, Clone)]
pub struct SemanticHit {
    pub identity_key: String,
    pub label: String,
    pub statement: String,
    pub score: f32,
    pub decl_kind: String,
    pub module_name: String,
}

/// Manages vector embeddings for the corpus.
pub struct EmbeddingStore {
    client: Arc<Qdrant>,
}

impl EmbeddingStore {
    /// Connect to a Qdrant instance. For local use, pass a path for on-disk storage.
    /// For cloud, pass a URL like "http://localhost:6334".
    pub async fn open_local(_storage_path: &Path) -> Result<Self> {
        let client = Qdrant::from_url("http://localhost:6334")
            .build()
            .map_err(|e| anyhow::anyhow!("Qdrant connection failed: {e}"))?;

        let store = Self {
            client: Arc::new(client),
        };
        store.ensure_collection().await?;
        Ok(store)
    }

    /// Connect to a remote Qdrant server.
    pub async fn open_remote(url: &str) -> Result<Self> {
        let client = Qdrant::from_url(url)
            .build()
            .map_err(|e| anyhow::anyhow!("Qdrant connection failed: {e}"))?;

        let store = Self {
            client: Arc::new(client),
        };
        store.ensure_collection().await?;
        Ok(store)
    }

    async fn ensure_collection(&self) -> Result<()> {
        let collections = self
            .client
            .list_collections()
            .await
            .map_err(|e| anyhow::anyhow!("listing collections: {e}"))?;

        let exists = collections
            .collections
            .iter()
            .any(|c| c.name == COLLECTION_NAME);

        if !exists {
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(COLLECTION_NAME)
                        .vectors_config(VectorParamsBuilder::new(EMBEDDING_DIM, Distance::Cosine)),
                )
                .await
                .map_err(|e| anyhow::anyhow!("creating collection: {e}"))?;
        }
        Ok(())
    }

    /// Upsert a verified corpus item into the vector store.
    pub async fn upsert_item(
        &self,
        identity_key: &str,
        label: &str,
        statement: &str,
        decl_kind: &str,
        module_name: &str,
        _artifact_content: &str,
        embedding: Vec<f32>,
    ) -> Result<()> {
        let mut payload = HashMap::new();
        payload.insert(
            "identity_key".to_string(),
            qdrant_client::qdrant::Value::from(identity_key.to_string()),
        );
        payload.insert(
            "label".to_string(),
            qdrant_client::qdrant::Value::from(label.to_string()),
        );
        payload.insert(
            "statement".to_string(),
            qdrant_client::qdrant::Value::from(statement.to_string()),
        );
        payload.insert(
            "decl_kind".to_string(),
            qdrant_client::qdrant::Value::from(decl_kind.to_string()),
        );
        payload.insert(
            "module_name".to_string(),
            qdrant_client::qdrant::Value::from(module_name.to_string()),
        );

        // Use a stable numeric ID from the identity_key hash
        let point_id = stable_point_id(identity_key);

        let point = PointStruct::new(point_id, embedding, payload);

        self.client
            .upsert_points(UpsertPointsBuilder::new(COLLECTION_NAME, vec![point]))
            .await
            .map_err(|e| anyhow::anyhow!("upserting point: {e}"))?;

        Ok(())
    }

    /// Search for similar items by embedding vector.
    pub async fn search(
        &self,
        query_embedding: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<SemanticHit>> {
        let results = self
            .client
            .search_points(
                SearchPointsBuilder::new(COLLECTION_NAME, query_embedding, limit as u64)
                    .with_payload(true),
            )
            .await
            .map_err(|e| anyhow::anyhow!("searching points: {e}"))?;

        let hits = results
            .result
            .into_iter()
            .map(|point| {
                let payload = &point.payload;
                SemanticHit {
                    identity_key: payload_string(payload, "identity_key"),
                    label: payload_string(payload, "label"),
                    statement: payload_string(payload, "statement"),
                    score: point.score,
                    decl_kind: payload_string(payload, "decl_kind"),
                    module_name: payload_string(payload, "module_name"),
                }
            })
            .collect();

        Ok(hits)
    }
}

fn payload_string(payload: &HashMap<String, qdrant_client::qdrant::Value>, key: &str) -> String {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn stable_point_id(identity_key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    identity_key.hash(&mut hasher);
    hasher.finish()
}

// --- Embedding generation via OpenAI API ---

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

/// Generate an embedding for a math text using OpenAI's embedding API.
/// Returns None if no API key is available (graceful offline fallback).
pub async fn generate_embedding(text: &str) -> Option<Vec<f32>> {
    let api_key = std::env::var("OPENAI_API_KEY").ok()?;

    let client = reqwest::Client::new();
    let response = client
        .post(OPENAI_EMBEDDING_URL)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "input": text,
            "model": EMBEDDING_MODEL,
        }))
        .send()
        .await
        .ok()?;

    if !response.status().is_success() {
        return None;
    }

    let body: EmbeddingResponse = response.json().await.ok()?;
    body.data.first().map(|d| d.embedding.clone())
}

/// Search for premises relevant to a given goal type.
/// Combines semantic search (embedding similarity) with the goal type text.
pub async fn search_premises_for_goal(
    store: &EmbeddingStore,
    goal_type: &str,
) -> Result<Vec<SemanticHit>> {
    let embedding = match generate_embedding(goal_type).await {
        Some(e) => e,
        None => return Ok(Vec::new()),
    };
    store.search(embedding, 10).await
}

/// Build the text to embed for a corpus item.
/// Combines the statement, label, and key metadata into a searchable string.
pub fn build_embedding_text(
    label: &str,
    statement: &str,
    decl_kind: &str,
    module_name: &str,
    artifact_content: &str,
) -> String {
    let mut parts = vec![
        format!("{decl_kind}: {label}"),
        statement.to_string(),
    ];
    if !module_name.is_empty() {
        parts.push(format!("module: {module_name}"));
    }
    // Include the first few lines of the proof for tactic context
    let proof_preview: String = artifact_content
        .lines()
        .filter(|l| !l.trim().starts_with("import") && !l.trim().starts_with("--"))
        .take(10)
        .collect::<Vec<_>>()
        .join("\n");
    if !proof_preview.trim().is_empty() {
        parts.push(proof_preview);
    }
    parts.join("\n")
}
