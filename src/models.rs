//! Shared LLM models: a model name, endpoint and token, written into a
//! project's `.env` as the provider's environment variables and handed to
//! sessions started there.

use crate::config::{config_path, load_toml_or_default, save_toml_to};
use crate::envvault::{self, EnvEntry};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::{Path, PathBuf},
};

/// Which family of environment variables a model is written as.
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Claude Code: `ANTHROPIC_MODEL`, `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`.
    #[default]
    Anthropic,
    /// Codex and OpenAI-compatible tools: `OPENAI_MODEL`, `OPENAI_BASE_URL`, `OPENAI_API_KEY`.
    Openai,
    /// Gemini CLI: `GEMINI_MODEL`, `GOOGLE_GEMINI_BASE_URL`, `GEMINI_API_KEY`.
    Gemini,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::Openai => "openai",
            Provider::Gemini => "gemini",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Provider::Anthropic => Provider::Openai,
            Provider::Openai => Provider::Gemini,
            Provider::Gemini => Provider::Anthropic,
        }
    }
    /// The (model, endpoint, token) variable names.
    pub fn keys(self) -> [&'static str; 3] {
        match self {
            Provider::Anthropic => [
                "ANTHROPIC_MODEL",
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_AUTH_TOKEN",
            ],
            Provider::Openai => ["OPENAI_MODEL", "OPENAI_BASE_URL", "OPENAI_API_KEY"],
            Provider::Gemini => ["GEMINI_MODEL", "GOOGLE_GEMINI_BASE_URL", "GEMINI_API_KEY"],
        }
    }
}

const PROVIDERS: [Provider; 3] = [Provider::Anthropic, Provider::Openai, Provider::Gemini];

/// True for a variable some model writes: sessions get these from `.env`.
pub fn is_model_key(key: &str) -> bool {
    PROVIDERS.iter().any(|p| p.keys().contains(&key))
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default)]
pub struct ModelEntry {
    pub name: String,
    #[serde(default)]
    pub provider: Provider,
    pub model: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub token: String,
}

impl ModelEntry {
    /// The variables this model writes; blank endpoint or token are left out.
    pub fn env(&self) -> Vec<EnvEntry> {
        let [mk, ek, tk] = self.provider.keys();
        [(mk, &self.model), (ek, &self.endpoint), (tk, &self.token)]
            .into_iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, v)| EnvEntry {
                key: k.to_string(),
                value: v.trim().to_string(),
            })
            .collect()
    }
    /// The project's `.env` holds exactly this model.
    pub fn applied(&self, dotenv: &BTreeMap<String, String>) -> bool {
        let [mk, ek, tk] = self.provider.keys();
        let want = self.env();
        [mk, ek, tk]
            .iter()
            .all(|k| want.iter().find(|e| e.key == *k).map(|e| &e.value) == dotenv.get(*k))
    }
}

#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ModelVault {
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

/// `models.toml` next to `config.toml` (tokens inside: written private).
pub fn models_path() -> Result<PathBuf> {
    if let Some(p) = env::var_os("AGENTSQUAD_MODELS").filter(|s| !s.is_empty()) {
        return Ok(p.into());
    }
    Ok(config_path()?.with_file_name("models.toml"))
}
pub fn load_models() -> Result<ModelVault> {
    load_toml_or_default(&models_path()?)
}
pub fn save_models(v: &ModelVault) -> Result<()> {
    save_toml_to(v, &models_path()?, true)
}

/// A model name: non-blank, no control characters.
pub fn validate_model_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("Name cannot be empty.".into());
    }
    if name.chars().any(char::is_control) {
        return Err("Name cannot hold control characters.".into());
    }
    Ok(name.to_string())
}

/// Names of the vault models the project's `.env` holds.
pub fn applied_names(vault: &ModelVault, dotenv: &BTreeMap<String, String>) -> BTreeSet<String> {
    vault
        .models
        .iter()
        .filter(|m| m.applied(dotenv))
        .map(|m| m.name.clone())
        .collect()
}

/// Write the picked models into `<dir>/.env` and clear the variables of
/// every provider that no longer has a picked model. One model per provider:
/// the later pick wins. Returns `(written, removed)` variable counts.
pub fn sync_project_models(
    dir: &Path,
    vault: &ModelVault,
    picked: &BTreeSet<String>,
    dotenv: &BTreeMap<String, String>,
) -> Result<(usize, usize)> {
    let chosen: Vec<&ModelEntry> = vault
        .models
        .iter()
        .filter(|m| picked.contains(&m.name))
        .collect();
    let mut write = Vec::new();
    let mut remove = Vec::new();
    for p in PROVIDERS {
        let want = match chosen.iter().rev().find(|m| m.provider == p) {
            Some(m) => m.env(),
            // Unpicked: clear the provider only when a vault model was there,
            // so hand-written keys nobody manages here survive.
            None if vault
                .models
                .iter()
                .any(|m| m.provider == p && m.applied(dotenv)) =>
            {
                Vec::new()
            }
            None => continue,
        };
        // Keys the model leaves blank are removed, not kept from before.
        remove.extend(
            p.keys()
                .iter()
                .filter(|k| dotenv.contains_key(**k) && !want.iter().any(|e| e.key == **k))
                .map(|k| k.to_string()),
        );
        write.extend(
            want.into_iter()
                .filter(|e| dotenv.get(&e.key) != Some(&e.value)),
        );
    }
    if write.is_empty() && remove.is_empty() {
        return Ok((0, 0));
    }
    let (a, u) = envvault::apply_dotenv(dir, &write)?;
    let r = envvault::remove_dotenv(dir, &remove)?;
    Ok((a + u, r))
}
