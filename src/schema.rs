use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::Context;
use jsonschema::Validator;
use referencing::{Retrieve, Uri};
use serde_json::Value as JsonValue;
use tracing::debug;
use url::Url;

use crate::yaml_json::yaml_to_json_value;

macro_rules! dprintln {
    ($($t:tt)*) => {
        if std::env::var_os("DEBUG").is_some() {
            eprintln!($($t)*);
        }
    };
}

#[derive(Debug, Clone, Copy)]
pub struct ServerConfig {
    pub validate_formats: bool,
    pub max_errors: usize,
    pub schema_cache_size: usize,
}

#[derive(Debug)]
struct RetrieverError(String);

impl std::fmt::Display for RetrieverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RetrieverError {}

#[derive(Clone)]
struct SchemaRetriever {
    client: reqwest::blocking::Client,
}

impl std::fmt::Debug for SchemaRetriever {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaRetriever").finish_non_exhaustive()
    }
}

impl SchemaRetriever {
    fn new() -> anyhow::Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent("cgpt-jsonschema-lsp/0.1")
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        Ok(Self { client })
    }

    fn retrieve_http(&self, uri: &str) -> anyhow::Result<JsonValue> {
        dprintln!("[DEBUG] retrieve_http: {uri}");

        let resp = self
            .client
            .get(uri)
            .send()
            .with_context(|| format!("GET {uri}"))?
            .error_for_status()
            .with_context(|| format!("GET {uri} (status)"))?;

        let bytes = resp.bytes().with_context(|| format!("GET {uri} (body)"))?;

        // Try JSON first, then YAML.
        if let Ok(v) = serde_json::from_slice::<JsonValue>(&bytes) {
            return Ok(v);
        }

        let yaml: serde_yaml::Value =
            serde_yaml::from_slice(&bytes).with_context(|| format!("parse YAML from {uri}"))?;
        Ok(yaml_to_json_value(&yaml)?)
    }

    fn retrieve_file(&self, url: &Url) -> anyhow::Result<JsonValue> {
        let path = url
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("not a valid file:// URL: {}", url))?;
        dprintln!("[DEBUG] retrieve_file: {}", path.display());

        let bytes = std::fs::read(&path).with_context(|| format!("read schema file: {path:?}"))?;

        if let Ok(v) = serde_json::from_slice::<JsonValue>(&bytes) {
            return Ok(v);
        }

        let yaml: serde_yaml::Value =
            serde_yaml::from_slice(&bytes).with_context(|| format!("parse YAML schema: {path:?}"))?;
        Ok(yaml_to_json_value(&yaml)?)
    }
}

impl Retrieve for SchemaRetriever {
    fn retrieve(
        &self,
        uri: &Uri<std::string::String>,
    ) -> Result<JsonValue, Box<dyn std::error::Error + Send + Sync>> {
        let s = uri.as_str();
        dprintln!("[DEBUG] retriever.retrieve: {s}");

        let url = Url::parse(s).map_err(|e| {
            Box::new(RetrieverError(format!("invalid schema URI: {s}: {e}")))
                as Box<dyn std::error::Error + Send + Sync>
        })?;

        match url.scheme() {
            "http" | "https" => self.retrieve_http(s).map_err(|e| {
                Box::new(RetrieverError(format!("failed to retrieve {s}: {e:#}"))) as _
            }),
            "file" => self.retrieve_file(&url).map_err(|e| {
                Box::new(RetrieverError(format!("failed to retrieve {s}: {e:#}"))) as _
            }),
            other => Err(Box::new(RetrieverError(format!(
                "unsupported schema URI scheme: {other} ({s})"
            ))) as _),
        }
    }
}

#[derive(Clone)]
struct CacheEntry {
    validator: Arc<Validator>,
    // Only tracked for the root schema URI.
    root_mtime: Option<SystemTime>,
}

impl std::fmt::Debug for CacheEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheEntry")
            .field("root_mtime", &self.root_mtime)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct SchemaCache {
    pub cfg: ServerConfig,
    retriever: SchemaRetriever,
    inner: Mutex<HashMap<String, CacheEntry>>,
}

impl SchemaCache {
    pub fn new(cfg: ServerConfig) -> Self {
        let retriever = SchemaRetriever::new().expect("failed to build reqwest client");
        Self {
            cfg,
            retriever,
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn validator_for_schema_uri(&self, schema_uri: &str) -> anyhow::Result<Arc<Validator>> {
        let root_mtime = schema_uri_mtime(schema_uri);

        {
            let guard = self.inner.lock().unwrap();
            if let Some(entry) = guard.get(schema_uri) {
                if entry.root_mtime.is_some() && entry.root_mtime == root_mtime {
                    dprintln!("[DEBUG] schema cache hit (file mtime ok): {schema_uri}");
                    return Ok(entry.validator.clone());
                }
                if entry.root_mtime.is_none() && root_mtime.is_none() {
                    dprintln!("[DEBUG] schema cache hit (non-file): {schema_uri}");
                    return Ok(entry.validator.clone());
                }
                dprintln!("[DEBUG] schema cache stale: {schema_uri}");
            } else {
                dprintln!("[DEBUG] schema cache miss: {schema_uri}");
            }
        }

        let validator = self.build_validator(schema_uri)?;
        let entry = CacheEntry {
            validator: Arc::new(validator),
            root_mtime,
        };

        let mut guard = self.inner.lock().unwrap();
        guard.insert(schema_uri.to_string(), entry);

        // Very simple cap to avoid unbounded growth.
        if guard.len() > self.cfg.schema_cache_size {
            if let Some(k) = guard.keys().next().cloned() {
                dprintln!("[DEBUG] schema cache evict: {k}");
                guard.remove(&k);
            }
        }

        Ok(guard
            .get(schema_uri)
            .expect("just inserted")
            .validator
            .clone())
    }

    fn build_validator(&self, schema_uri: &str) -> anyhow::Result<Validator> {
        // Build a tiny schema that references the external schema by URI.
        let wrapper = serde_json::json!({ "$ref": schema_uri });

        let mut opts = jsonschema::options().with_retriever(self.retriever.clone());
        if self.cfg.validate_formats {
            opts = opts.should_validate_formats(true);
        }

        let validator = opts
            .build(&wrapper)
            .with_context(|| format!("build schema validator for {schema_uri}"))?;

        debug!("compiled schema: {schema_uri}");
        dprintln!("[DEBUG] compiled schema: {schema_uri}");
        Ok(validator)
    }
}

fn schema_uri_mtime(schema_uri: &str) -> Option<SystemTime> {
    let Ok(mut url) = Url::parse(schema_uri) else {
        return None;
    };
    if url.scheme() != "file" {
        return None;
    }
    url.set_fragment(None);
    let Ok(path) = url.to_file_path() else {
        return None;
    };
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Resolve a `$schema` string from an instance file to an absolute URI that the validator can fetch.
///
/// - `https://...` stays as is
/// - `file:///...` stays as is
/// - `./relative/path.json` becomes `file:///ABS/.../relative/path.json`
/// - fragments like `.../schema.json#/defs/Foo` are preserved
pub fn resolve_schema_uri(instance_uri: &Url, raw_schema: &str) -> anyhow::Result<String> {
    let raw_schema = raw_schema.trim();
    if raw_schema.is_empty() {
        anyhow::bail!("empty $schema");
    }

    // Already an absolute URL?
    if raw_schema.starts_with("http://")
        || raw_schema.starts_with("https://")
        || raw_schema.starts_with("file://")
    {
        dprintln!("[DEBUG] resolve_schema_uri: already absolute: {raw_schema}");
        return Ok(raw_schema.to_string());
    }

    // Treat as local path (absolute or relative to the instance file).
    let (path_part, frag_part) = match raw_schema.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (raw_schema, None),
    };

    let base_path = instance_uri
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("instance is not a file:// URI: {instance_uri}"))?;
    let base_dir = base_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("instance has no parent dir: {base_path:?}"))?;

    let schema_path = std::path::Path::new(path_part);
    let abs = if schema_path.is_absolute() {
        schema_path.to_path_buf()
    } else {
        base_dir.join(schema_path)
    };

    // Canonicalize only if it exists, to avoid losing "missing file" info.
    let abs = std::fs::canonicalize(&abs).unwrap_or(abs);

    let mut url = Url::from_file_path(&abs)
        .map_err(|_| anyhow::anyhow!("failed to convert to file:// URL: {abs:?}"))?;
    if let Some(frag) = frag_part {
        url.set_fragment(Some(frag));
    }

    let out = url.to_string();
    dprintln!("[DEBUG] resolve_schema_uri: {raw_schema} -> {out}");
    Ok(out)
}
