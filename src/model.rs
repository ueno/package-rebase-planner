//! Model access for the reconciliation agent.
//!
//! The agent talks to this module's `Backend` trait, never to a provider
//! directly. That keeps the pipeline testable without an API key.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use goose_providers::api_client::TlsConfig;
use goose_providers::base::Provider;
use goose_providers::conversation::message::Message;
use goose_providers::declarative::{
    self, DeclarativeProviderConfig, EnvKeyResolver, KeyResolver, ProviderEngine,
};
use goose_providers::model::ModelConfig;
use goose_providers::{anthropic, ollama, openai};
use rmcp::model::Tool;
use std::sync::Arc;

/// What a model call cost, for run logs.
#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
}

/// One inference call. Async because `goose_agent`'s `Inference` trait is.
#[async_trait]
pub trait Backend: Send + Sync {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<(Message, Usage)>;
}

/// A backend that refuses to run, so "no model configured" is a clear error
/// rather than a network failure.
pub struct StubBackend;

#[async_trait]
impl Backend for StubBackend {
    async fn complete(
        &self,
        _system: &str,
        _messages: &[Message],
        _tools: &[Tool],
    ) -> Result<(Message, Usage)> {
        bail!(
            "no model is configured.\n\
             Set [model] provider = \"anthropic\" in config.toml and export ANTHROPIC_API_KEY."
        )
    }
}

/// Declarative model via `goose-providers`.
pub struct DeclarativeBackend {
    provider: Box<dyn Provider>,
    config: ModelConfig,
}

impl DeclarativeBackend {
    pub fn new(
        config: DeclarativeProviderConfig,
        model_name: &str,
        tls_config: Option<TlsConfig>,
        key_resolver: impl KeyResolver,
    ) -> Result<Self> {
        let provider = match config.engine {
            ProviderEngine::OpenAI => {
                openai::from_declarative_config(config, tls_config, key_resolver)
                    .map(|provider| Box::new(provider.build()) as Box<dyn Provider>)?
            }
            ProviderEngine::Ollama => {
                ollama::from_declarative_config(config, tls_config, key_resolver)
                    .map(|provider| Box::new(provider.build()) as Box<dyn Provider>)?
            }
            ProviderEngine::Anthropic => {
                anthropic::from_declarative_config(config, tls_config, key_resolver)
                    .map(|provider| Box::new(provider.build()) as Box<dyn Provider>)?
            }
        };

        Ok(Self {
            provider,
            config: ModelConfig::new(model_name),
        })
    }
}

#[async_trait]
impl Backend for DeclarativeBackend {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<(Message, Usage)> {
        let (reply, usage) = self
            .provider
            .complete(&self.config, system, messages, tools)
            .await
            .context("model request failed")?;
        Ok((
            reply,
            Usage {
                input_tokens: usage.usage.input_tokens,
                output_tokens: usage.usage.output_tokens,
            },
        ))
    }
}

pub const DEFAULT_DECLARATIVE_MODEL: &str = "ramalama";

/// Which agent a model is being built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Assess,
    Plan,
}

impl Task {
    pub fn name(self) -> &'static str {
        match self {
            Task::Assess => "assess",
            Task::Plan => "plan",
        }
    }
}

/// Settings for one task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSettings {
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Empty means "the provider's own default".
    #[serde(default)]
    pub name: String,
}

fn default_provider() -> String {
    "anthropic".to_string()
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            name: String::new(),
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
struct ConfigFile {
    #[serde(default)]
    assess: Option<ModelSettings>,
    #[serde(default)]
    plan: Option<ModelSettings>,
}

impl ModelSettings {
    /// Override the provider, e.g. from `--local`.
    ///
    /// The name is cleared with it: a model name means something only to the
    /// provider it was written for.
    #[cfg(test)]
    pub fn with_provider(&self, provider: &str) -> Self {
        Self {
            provider: provider.to_string(),
            name: String::new(),
        }
    }

    /// Load settings for a task from `<home>/config.toml`.
    pub fn load(home: &std::path::Path, task: Task) -> Result<Self> {
        let path = home.join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let parsed: ConfigFile = toml::from_str(&raw).with_context(|| {
            format!(
                "{} is not valid TOML; needed to resolve the model for `lw {}`",
                path.display(),
                task.name()
            )
        })?;
        let chosen = match task {
            Task::Assess => parsed.assess,
            Task::Plan => parsed.plan,
        };
        Ok(chosen.unwrap_or_default())
    }

    /// A short description, e.g. `anthropic/claude-opus-5`.
    #[cfg(test)]
    pub fn describe(&self) -> String {
        if self.name.is_empty() {
            format!("{} (default model)", self.provider)
        } else {
            format!("{}/{}", self.provider, self.name)
        }
    }
}

pub fn build(settings: &ModelSettings, home: &std::path::Path) -> Result<Arc<dyn Backend>> {
    let _ = (home,);

    let mut providers = declarative::fixed_provider_configs()?;
    let path = home.join("providers");
    if path.exists() {
        providers.append(&mut declarative::load_custom_providers(&path)?);
    }

    match settings.provider.as_str() {
        "none" | "stub" => Ok(Arc::new(StubBackend)),

        other => {
            if let Some(provider) = providers
                .into_iter()
                .find(|provider| provider.name == other)
            {
                let name = if settings.name.is_empty() {
                    DEFAULT_DECLARATIVE_MODEL
                } else {
                    &settings.name
                };
                Ok(Arc::new(DeclarativeBackend::new(
                    provider,
                    name,
                    None,
                    EnvKeyResolver::new(),
                )?))
            } else {
                bail!("unknown model provider `{other}`; expected `anthropic`, `local`, or `none`")
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_refuses_and_says_why() {
        let error = StubBackend
            .complete("s", &[], &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("no model is configured"), "{error}");
        assert!(error.contains("config.toml"), "{error}");
    }

    /// Guards against a silent misconfiguration: `goose-providers` defaults to
    /// no TLS backend, and without one every HTTPS request fails as a
    /// "could not connect" network error that looks like the user's fault.
    /// Constructing a client is enough to catch it — reqwest panics at build
    /// time when asked for TLS it wasn't compiled with.
    #[tokio::test]
    async fn https_is_supported() {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("no TLS backend compiled in; check goose-providers features");
        // A HEAD to a known-good endpoint: any HTTP status proves TLS works.
        // Only the transport is under test, so a network failure is tolerated.
        if let Ok(response) = client
            .head("https://api.anthropic.com/v1/models")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            assert!(response.status().as_u16() > 0);
        }
    }

    fn settings(toml: &str, task: Task) -> ModelSettings {
        let dir = tempdir::TempDir::new("settings").expect("tempdir should be created");
        std::fs::write(dir.path().join("config.toml"), toml).unwrap();
        ModelSettings::load(dir.path(), task).unwrap()
    }

    #[test]
    fn each_task_reads_its_own_table() {
        let toml = "[assess]\nprovider = \"local\"\n";
        let assess = settings(toml, Task::Assess);
        assert_eq!(assess.provider, "local");
        assert_eq!(assess.name, "", "local picks its own default");
    }

    #[test]
    fn a_missing_table_falls_back_to_defaults() {
        let toml = "";
        assert_eq!(settings(toml, Task::Assess), ModelSettings::default());
        assert_eq!(settings(toml, Task::Plan), ModelSettings::default());
    }

    #[test]
    fn an_absent_config_still_works() {
        let dir = tempdir::TempDir::new("an_absent_config_still_works")
            .expect("tempdir should be created");
        assert_eq!(
            ModelSettings::load(dir.path(), Task::Assess).unwrap(),
            ModelSettings::default()
        );
    }

    /// A typo would otherwise be silently ignored, leaving the user wondering
    /// why their setting did nothing.
    #[test]
    fn an_unknown_key_is_an_error() {
        let dir =
            tempdir::TempDir::new("an_unknown_key_is_an_error").expect("tempdir should be created");
        std::fs::write(
            dir.path().join("config.toml"),
            "[assess]\nprovder = \"local\"\n",
        )
        .unwrap();
        let error = ModelSettings::load(dir.path(), Task::Assess)
            .unwrap_err()
            .to_string();
        assert!(error.contains("config.toml"), "{error}");
    }

    #[test]
    fn with_provider_clears_the_name() {
        let settings = ModelSettings {
            provider: "anthropic".into(),
            name: "claude-opus-5".into(),
        };
        let local = settings.with_provider("local");
        assert_eq!(local.provider, "local");
        assert_eq!(local.name, "");
    }

    #[test]
    fn describe_names_the_provider_and_model() {
        assert_eq!(
            ModelSettings {
                provider: "local".into(),
                name: String::new(),
            }
            .describe(),
            "local (default model)"
        );
    }
}
