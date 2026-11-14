//! Protected startup-only media enrollment for the first production adapter.
//!
//! The native protocol receives opaque source/item references only. Upstream
//! addresses, credentials, process paths and output custody stay in this
//! owner-protected document and are converted to typed server configuration.

#[cfg(unix)]
use std::path::Component;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use ottplay_core::multiview::{
    broker::{
        network::{CheckedTransport, Limits, NetworkPolicy, OriginCredentials, OriginRule},
        registry::{
            self, Enrollment as RegistryEnrollment, ItemEnrollment, Registry, SourcePrincipal,
        },
        resolvers::ServerSource,
        runtime::{SourceConfig, SourceService},
        wire::{
            self, Canvas, Descriptor, GeometryMode, OutputProfile, Permission, Principal,
            Protection,
        },
    },
    frames::FrameSpec,
    persistent,
};
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Url,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::multiview_broker::{BackendConfig, CredentialConfig, OperatorConfig};

const MEDIA_VERSION: u8 = 1;
const ADAPTER: &str = "http-mpegts-v1";
const HLS_ADAPTER: &str = "http-hls-ts-v1";
fn default_input_adapter() -> String {
    ADAPTER.into()
}
const MAX_ITEMS: usize = 256;
const MAX_INPUTS: u8 = 9;
const MAX_ALLOWED: usize = 128;
const MAX_ORIGINS: usize = 32;
const MAX_DURATION_SECS: u64 = 24 * 60 * 60;
const MAX_BEARER_BYTES: usize = 4096;

fn invalid(reason: &'static str) -> anyhow::Error {
    anyhow::anyhow!("multiview media configuration: {reason}")
}

/// Parsed optional production-media opt-in. Secret-bearing values deliberately
/// have no `Debug` or `Serialize` implementation.
pub(crate) struct MediaDocument {
    version: u8,
    adapter: String,
    source_ref: String,
    profile_uuid: String,
    config_revision: String,
    policy_revision: String,
    allowed: Vec<AllowedDocument>,
    network: Vec<NetworkDocument>,
    items: Vec<ItemDocument>,
    output: OutputDocument,
    max_duration_seconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawMediaDocument {
    version: u8,
    adapter: String,
    source_ref: String,
    profile_uuid: String,
    config_revision: String,
    policy_revision: String,
    allowed: Vec<AllowedDocument>,
    network: Vec<NetworkDocument>,
    items: Vec<ItemDocument>,
    output: OutputDocument,
    max_duration_seconds: u64,
}

// Keep the deserialization representation private to this module; in
// particular, none of the secret-bearing documents derive Debug.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AllowedDocument {
    subject: String,
    installation_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NetworkDocument {
    origin: String,
    #[serde(default)]
    private_addresses: Vec<IpAddr>,
    #[serde(default)]
    allow_public_http: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ItemDocument {
    item_ref: String,
    #[serde(default = "default_input_adapter")]
    input_adapter: String,
    favorite_sha256: String,
    title: String,
    provider: String,
    url: String,
    #[serde(default)]
    bearer_token: Option<String>,
    #[serde(default)]
    allow_cleartext_credentials: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputDocument {
    root: PathBuf,
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    mkfifo: PathBuf,
}

impl From<RawMediaDocument> for MediaDocument {
    fn from(value: RawMediaDocument) -> Self {
        Self {
            version: value.version,
            adapter: value.adapter,
            source_ref: value.source_ref,
            profile_uuid: value.profile_uuid,
            config_revision: value.config_revision,
            policy_revision: value.policy_revision,
            allowed: value.allowed,
            network: value.network,
            items: value.items,
            output: value.output,
            max_duration_seconds: value.max_duration_seconds,
        }
    }
}

/// The result of loading one protected operator file. No capability is enabled
/// unless `media` is present and successfully prepared by the caller.
pub(crate) struct LoadedOperatorConfig {
    pub(crate) operator: OperatorConfig,
    pub(crate) media: Option<MediaConfig>,
}

pub(crate) struct MediaConfig {
    source_ref: String,
    profile_uuid: Uuid,
    config_revision: String,
    policy_revision: String,
    allowed: BTreeSet<SourcePrincipal>,
    items: Vec<ItemDocument>,
    network: Vec<NetworkDocument>,
    output: OutputDocument,
    max_duration: Duration,
}

pub(crate) struct MediaPlan {
    registry: Arc<Registry>,
    backend: Option<BackendConfig>,
    enrollment: Option<RegistryEnrollment>,
    configurations: Option<BTreeMap<String, SourceConfig>>,
    server_id: String,
}

impl MediaDocument {
    pub(crate) fn from_raw(value: RawMediaDocument) -> Self {
        value.into()
    }

    pub(crate) fn validate(self, operator: &OperatorConfig) -> anyhow::Result<MediaConfig> {
        if self.version != MEDIA_VERSION
            || self.adapter != ADAPTER
            || !wire::opaque(&self.source_ref)
            || !wire::opaque(&self.config_revision)
            || !wire::opaque(&self.policy_revision)
            || self.items.is_empty()
            || self.items.len() > MAX_ITEMS
            || self.allowed.is_empty()
            || self.allowed.len() > MAX_ALLOWED
            || self.network.is_empty()
            || self.network.len() > MAX_ORIGINS
            || self.max_duration_seconds == 0
            || self.max_duration_seconds > MAX_DURATION_SECS
        {
            return Err(invalid("unsupported or out-of-bounds opt-in"));
        }
        let profile_uuid = Uuid::parse_str(&self.profile_uuid)
            .ok()
            .filter(|value| !value.is_nil())
            .ok_or_else(|| invalid("invalid profile identity"))?;

        let credentials: BTreeSet<_> = operator
            .credentials
            .iter()
            .map(|credential: &CredentialConfig| {
                (
                    credential.subject.as_str(),
                    credential.enrollment.installation_id.as_str(),
                )
            })
            .collect();
        let mut allowed = BTreeSet::new();
        for item in &self.allowed {
            if !wire::opaque(&item.subject)
                || !wire::opaque(&item.installation_id)
                || !credentials.contains(&(item.subject.as_str(), item.installation_id.as_str()))
                || !allowed.insert(SourcePrincipal {
                    subject: item.subject.clone(),
                    installation_id: item.installation_id.clone(),
                })
            {
                return Err(invalid("invalid source audience"));
            }
        }

        let mut origins = BTreeMap::new();
        for rule in &self.network {
            if rule.private_addresses.len() > 64
                || rule
                    .private_addresses
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .len()
                    != rule.private_addresses.len()
            {
                return Err(invalid("invalid network policy"));
            }
            let origin = Url::parse(&rule.origin).map_err(|_| invalid("invalid network policy"))?;
            if origin.origin().ascii_serialization() != rule.origin
                || origin.path() != "/"
                || origin.query().is_some()
                || rule.allow_public_http
                    && (origin.scheme() != "http" || !rule.private_addresses.is_empty())
                || origins
                    .insert(
                        rule.origin.clone(),
                        OriginRule::new(&rule.origin, &rule.private_addresses)
                            .map_err(|_| invalid("invalid network policy"))?,
                    )
                    .is_some()
            {
                return Err(invalid("invalid network policy"));
            }
        }

        let mut seen_items = BTreeSet::new();
        let mut used_origins = BTreeSet::new();
        for item in &self.items {
            if !matches!(item.input_adapter.as_str(), ADAPTER | HLS_ADAPTER)
                || !wire::opaque(&item.item_ref)
                || !wire::favorite_digest(&item.favorite_sha256)
                || item.title.is_empty()
                || item.title.len() > 512
                || item.title.chars().any(char::is_control)
                || !wire::opaque(&item.provider)
                || item.url.len() > 8192
                || !seen_items.insert(item.item_ref.as_str())
            {
                return Err(invalid("invalid source item"));
            }
            let url = Url::parse(&item.url).map_err(|_| invalid("invalid source item"))?;
            let origin = url.origin().ascii_serialization();
            let rule = self
                .network
                .iter()
                .find(|rule| rule.origin == origin)
                .ok_or_else(|| invalid("source origin is not enrolled"))?;
            used_origins.insert(origin);
            if url.username() != "" || url.password().is_some() || url.fragment().is_some() {
                return Err(invalid("invalid source item"));
            }
            let literal = url.host_str().and_then(|host| {
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<IpAddr>()
                    .ok()
            });
            let private_literal = literal.filter(|address| private_ip(*address));
            if private_literal.is_some_and(|address| !rule.private_addresses.contains(&address)) {
                return Err(invalid("private source address is not enrolled"));
            }
            if url.scheme() == "http" {
                // Public HTTP is a protected per-origin opt-in. Its empty
                // private allowlist still requires every DNS answer to be public.
                // Otherwise preserve the explicitly enrolled private-literal path.
                if !rule.allow_public_http {
                    let peer = private_literal.ok_or_else(|| invalid("cleartext source denied"))?;
                    if !rule.private_addresses.contains(&peer) {
                        return Err(invalid("cleartext source denied"));
                    }
                }
            } else if url.scheme() != "https" {
                return Err(invalid("unsupported source transport"));
            }
            if let Some(token) = &item.bearer_token {
                if token.is_empty()
                    || token.len() > MAX_BEARER_BYTES
                    || !token.is_ascii()
                    || !token.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
                    })
                    || url.scheme() == "http" && !item.allow_cleartext_credentials
                {
                    return Err(invalid("invalid source credential policy"));
                }
            } else if item.allow_cleartext_credentials {
                return Err(invalid("invalid source credential policy"));
            }
        }
        if used_origins.len() != origins.len() {
            return Err(invalid("unused network policy"));
        }
        // Validate the combined policy now, before any listener can be bound.
        NetworkPolicy::new(origins.into_values().collect())
            .map_err(|_| invalid("invalid network policy"))?;
        Ok(MediaConfig {
            source_ref: self.source_ref,
            profile_uuid,
            config_revision: self.config_revision,
            policy_revision: self.policy_revision,
            allowed,
            items: self.items,
            network: self.network,
            output: self.output,
            max_duration: Duration::from_secs(self.max_duration_seconds),
        })
    }
}

impl MediaConfig {
    pub(crate) fn prepare(&self, operator: &OperatorConfig) -> anyhow::Result<MediaPlan> {
        #[cfg(not(unix))]
        return Err(invalid(
            "production adapter is unsupported on this platform",
        ));
        #[cfg(unix)]
        {
            validate_output_paths(&self.output)?;
            let profile = OutputProfile {
                profile_id: "http-mpegts-v1-640x360-10fps".into(),
                canvas: Canvas {
                    width: 640,
                    height: 360,
                    fps: 10,
                },
                max_inputs: MAX_INPUTS,
                geometry_modes: vec![GeometryMode::Disjoint, GeometryMode::Pip],
            };
            let backend = BackendConfig {
                media: persistent::Config {
                    ffmpeg: resolve_tool(&self.output.ffmpeg)?,
                    ffprobe: resolve_tool(&self.output.ffprobe)?,
                    mkfifo: resolve_tool(&self.output.mkfifo)?,
                    output_root: validate_output_root(&self.output.root)?,
                    frame_spec: FrameSpec {
                        width: 320,
                        height: 180,
                    },
                    startup_timeout: Duration::from_secs(10),
                    stale_after: Duration::from_secs(2),
                    max_duration: self.max_duration,
                    idle_timeout: Duration::from_secs(90),
                    max_disk_bytes: 128 * 1024 * 1024,
                    frame_memory_budget: 16 * 1024 * 1024,
                    max_input_pixel_rate: 64_000_000,
                    require_broker_grants: true,
                },
                profile,
                input_adapters: self
                    .items
                    .iter()
                    .map(|item| item.input_adapter.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            };
            let registry = Arc::new(
                Registry::new(operator.server_id.clone(), operator.server_epoch.clone())
                    .map_err(|_| invalid("cannot initialize source registry"))?,
            );
            let network_rules = self
                .network
                .iter()
                .map(|rule| OriginRule::new(&rule.origin, &rule.private_addresses))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| invalid("invalid network policy"))?;
            let policy =
                NetworkPolicy::new(network_rules).map_err(|_| invalid("invalid network policy"))?;
            let transport = CheckedTransport::new(
                policy,
                Limits {
                    body_bytes: 8 * 1024 * 1024,
                    request_bytes: 64 * 1024,
                    timeout: Duration::from_secs(8),
                    concurrent: usize::from(MAX_INPUTS),
                    redirects: 0,
                },
            )
            .map_err(|_| invalid("invalid transport limits"))?;

            let mut configurations = BTreeMap::new();
            let mut items = Vec::with_capacity(self.items.len());
            for item in &self.items {
                let credentials = item
                    .bearer_token
                    .as_ref()
                    .map(|token| {
                        let mut headers = HeaderMap::new();
                        let value = HeaderValue::from_str(&format!("Bearer {token}"))
                            .map_err(|_| invalid("invalid source credential policy"))?;
                        headers.insert(AUTHORIZATION, value);
                        OriginCredentials::new(&item.url, headers)
                            .map_err(|_| invalid("invalid source credential policy"))
                    })
                    .transpose()?;
                let source = match item.input_adapter.as_str() {
                    ADAPTER => ServerSource::http(&item.url, credentials),
                    HLS_ADAPTER => ServerSource::hls_ts(&item.url, credentials),
                    _ => return Err(invalid("unsupported source adapter")),
                }
                .map_err(|_| invalid("invalid source address"))?;
                configurations.insert(
                    item.item_ref.clone(),
                    SourceConfig {
                        source,
                        transport: transport.clone(),
                    },
                );
                items.push(ItemEnrollment {
                    item_ref: item.item_ref.clone(),
                    identity_version: 1,
                    favorite_sha256: item.favorite_sha256.clone(),
                    kind: "live".into(),
                    input_adapter: item.input_adapter.clone(),
                    protection: Protection {
                        known: true,
                        restricted: false,
                    },
                    descriptor: Descriptor {
                        title: item.title.clone(),
                        provider: item.provider.clone(),
                    },
                    input: registry::EnrolledInput { config_index: 0 },
                });
            }
            let enrollment = RegistryEnrollment {
                source_ref: self.source_ref.clone(),
                profile_uuid: self.profile_uuid,
                config_revision: self.config_revision.clone(),
                policy_revision: self.policy_revision.clone(),
                allowed: self.allowed.clone(),
                items,
            };
            Ok(MediaPlan {
                registry,
                backend: Some(backend),
                enrollment: Some(enrollment),
                configurations: Some(configurations),
                server_id: operator.server_id.clone(),
            })
        }
    }
}

impl MediaPlan {
    pub(crate) fn registry(&self) -> Arc<Registry> {
        Arc::clone(&self.registry)
    }

    pub(crate) fn take_backend(&mut self) -> anyhow::Result<BackendConfig> {
        self.backend
            .take()
            .ok_or_else(|| invalid("media plan already consumed"))
    }

    /// Apply trusted startup enrollment to the exact SourceService returned by
    /// Broker::new_live. The SourcesManage principal is ephemeral and never
    /// derived from or exposed to a wire credential.
    pub(crate) fn apply(&mut self, service: &SourceService) -> anyhow::Result<()> {
        let enrollment = self
            .enrollment
            .take()
            .ok_or_else(|| invalid("media plan already consumed"))?;
        let configurations = self
            .configurations
            .take()
            .ok_or_else(|| invalid("media plan already consumed"))?;
        let principal = Principal {
            subject: "multiview-startup-enroller".into(),
            installation_id: self.server_id.clone(),
            permissions: BTreeSet::from([Permission::SourcesManage]),
        };
        service
            .enroll(&principal, enrollment, configurations, None)
            .map_err(|_| invalid("cannot enroll configured media source"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multiview_broker::{Enrollment, ListenerPolicy};

    fn operator() -> OperatorConfig {
        OperatorConfig {
            server_id: "server-1".into(),
            server_epoch: "epoch-1".into(),
            origin: "https://multiview.example".into(),
            listener: ListenerPolicy::default(),
            allowed_origins: vec!["https://multiview.example".into()],
            signing_key: vec![b'k'; 32],
            credentials: vec![CredentialConfig {
                enrollment: Enrollment {
                    server_id: "server-1".into(),
                    origin: "https://multiview.example".into(),
                    audience: wire::AUDIENCE.into(),
                    installation_id: "installation-1".into(),
                    control_bearer: "control-token-never-logged-000001".into(),
                },
                subject: "subject-1".into(),
                permissions: BTreeSet::from([Permission::MultiviewControl]),
                revision: 1,
                expires_at_ms: u64::MAX,
            }],
        }
    }

    fn document(url: &str, origin: &str, private_addresses: Vec<IpAddr>) -> MediaDocument {
        MediaDocument {
            version: MEDIA_VERSION,
            adapter: ADAPTER.into(),
            source_ref: "source-1".into(),
            profile_uuid: "a2a7ff00-0000-4000-8000-000000000001".into(),
            config_revision: "config-1".into(),
            policy_revision: "policy-1".into(),
            allowed: vec![AllowedDocument {
                subject: "subject-1".into(),
                installation_id: "installation-1".into(),
            }],
            network: vec![NetworkDocument {
                origin: origin.into(),
                private_addresses,
                allow_public_http: false,
            }],
            items: vec![ItemDocument {
                input_adapter: ADAPTER.into(),
                item_ref: "item-1".into(),
                favorite_sha256: "a".repeat(64),
                title: "News".into(),
                provider: "Operator".into(),
                url: url.into(),
                bearer_token: None,
                allow_cleartext_credentials: false,
            }],
            output: OutputDocument {
                root: "/private/operator/multiview-output".into(),
                ffmpeg: "/usr/bin/ffmpeg".into(),
                ffprobe: "/usr/bin/ffprobe".into(),
                mkfifo: "/usr/bin/mkfifo".into(),
            },
            max_duration_seconds: 3600,
        }
    }

    #[test]
    fn opt_in_document_requires_exact_operator_audience_and_https_origin() {
        assert!(document(
            "https://media.example/live.ts",
            "https://media.example",
            vec![]
        )
        .validate(&operator())
        .is_ok());

        let mut unconfigured = document(
            "https://media.example/live.ts",
            "https://media.example",
            vec![],
        );
        unconfigured.allowed[0].subject = "other-subject".into();
        assert!(unconfigured.validate(&operator()).is_err());

        let mut unknown_adapter = document(
            "https://media.example/live.ts",
            "https://media.example",
            vec![],
        );
        unknown_adapter.adapter = "http-anything-v1".into();
        assert!(unknown_adapter.validate(&operator()).is_err());
    }

    #[test]
    fn cleartext_is_limited_to_explicit_private_ip_and_explicit_credential_opt_in() {
        assert!(document(
            "http://192.168.1.20:8080/live.ts",
            "http://192.168.1.20:8080",
            vec!["192.168.1.20".parse().unwrap()],
        )
        .validate(&operator())
        .is_ok());

        let dns = document(
            "http://media.lan:8080/live.ts",
            "http://media.lan:8080",
            vec!["192.168.1.20".parse().unwrap()],
        );
        assert!(dns.validate(&operator()).is_err());

        let unlisted = document(
            "http://192.168.1.20:8080/live.ts",
            "http://192.168.1.20:8080",
            vec![],
        );
        assert!(unlisted.validate(&operator()).is_err());

        let https_unlisted = document(
            "https://192.168.1.20:8443/live.ts",
            "https://192.168.1.20:8443",
            vec![],
        );
        assert!(https_unlisted.validate(&operator()).is_err());
        let https_unlisted = document(
            "https://192.168.1.20:8443/live.ts",
            "https://192.168.1.20:8443",
            vec!["192.168.1.20".parse().unwrap()],
        );
        assert!(https_unlisted.validate(&operator()).is_ok());

        let mut secret = document(
            "http://192.168.1.20:8080/live.ts",
            "http://192.168.1.20:8080",
            vec!["192.168.1.20".parse().unwrap()],
        );
        secret.items[0].bearer_token = Some("secret-token-1234567890".into());
        assert!(secret.validate(&operator()).is_err());
        let mut secret = document(
            "http://192.168.1.20:8080/live.ts",
            "http://192.168.1.20:8080",
            vec!["192.168.1.20".parse().unwrap()],
        );
        secret.items[0].bearer_token = Some("secret-token-1234567890".into());
        secret.items[0].allow_cleartext_credentials = true;
        assert!(secret.validate(&operator()).is_ok());
    }

    #[test]
    fn network_rules_are_exact_unique_and_all_used() {
        let mut unused = document(
            "https://media.example/live.ts",
            "https://media.example",
            vec![],
        );
        unused.network.push(NetworkDocument {
            origin: "https://unused.example".into(),
            private_addresses: vec![],
            allow_public_http: false,
        });
        assert!(unused.validate(&operator()).is_err());

        let mut duplicate_item = document(
            "https://media.example/live.ts",
            "https://media.example",
            vec![],
        );
        duplicate_item.items.push(ItemDocument {
            input_adapter: ADAPTER.into(),
            item_ref: "item-1".into(),
            favorite_sha256: "b".repeat(64),
            title: "Duplicate".into(),
            provider: "Operator".into(),
            url: "https://media.example/other.ts".into(),
            bearer_token: None,
            allow_cleartext_credentials: false,
        });
        assert!(duplicate_item.validate(&operator()).is_err());
    }

    #[test]
    fn public_http_is_protected_exact_origin_opt_in_and_keeps_bearer_consent() {
        for url in ["http://media.example/live.ts", "http://8.8.8.8/live.ts"] {
            let origin = Url::parse(url).unwrap().origin().ascii_serialization();
            assert!(document(url, &origin, vec![])
                .validate(&operator())
                .is_err());
            let mut allowed = document(url, &origin, vec![]);
            allowed.network[0].allow_public_http = true;
            assert!(allowed.validate(&operator()).is_ok());
            let mut denied = document(url, &origin, vec![]);
            denied.network[0].allow_public_http = true;
            denied.items[0].bearer_token = Some("synthetic-secret-token".into());
            let error = denied.validate(&operator()).err().unwrap().to_string();
            assert!(!error.contains(url) && !error.contains("synthetic-secret-token"));
            let mut credential = document(url, &origin, vec![]);
            credential.network[0].allow_public_http = true;
            credential.items[0].bearer_token = Some("synthetic-secret-token".into());
            credential.items[0].allow_cleartext_credentials = true;
            assert!(credential.validate(&operator()).is_ok());
        }
        for url in [
            "http://other.example/live.ts",
            "http://media.example:8080/live.ts",
            "http://child.media.example/live.ts",
            "http://user@media.example/live.ts",
            "http://media.example/live.ts#fragment",
        ] {
            let mut denied = document(url, "http://media.example", vec![]);
            denied.network[0].allow_public_http = true;
            assert!(denied.validate(&operator()).is_err());
        }
    }

    #[test]
    fn public_http_rejects_https_and_any_private_rule_even_when_unused() {
        let mut https = document(
            "https://media.example/live.ts",
            "https://media.example",
            vec![],
        );
        https.network[0].allow_public_http = true;
        assert!(https.validate(&operator()).is_err());
        let private = "192.168.1.20".parse().unwrap();
        let mut denied = document(
            "http://media.example/live.ts",
            "http://media.example",
            vec![private],
        );
        denied.network[0].allow_public_http = true;
        assert!(denied.validate(&operator()).is_err());
        let mut unused = document(
            "https://media.example/live.ts",
            "https://media.example",
            vec![],
        );
        unused.network.push(NetworkDocument {
            origin: "http://unused.example".into(),
            private_addresses: vec![private],
            allow_public_http: true,
        });
        assert!(unused.validate(&operator()).is_err());
        let mut no_token = document(
            "http://media.example/live.ts",
            "http://media.example",
            vec![],
        );
        no_token.network[0].allow_public_http = true;
        no_token.items[0].allow_cleartext_credentials = true;
        assert!(no_token.validate(&operator()).is_err());
    }

    #[test]
    fn public_http_network_flag_is_closed_boolean_and_defaults_false() {
        let absent: NetworkDocument =
            serde_json::from_value(serde_json::json!({"origin":"http://media.example"})).unwrap();
        assert!(!absent.allow_public_http);
        let explicit: NetworkDocument = serde_json::from_value(
            serde_json::json!({"origin":"http://media.example", "allow_public_http":false}),
        )
        .unwrap();
        assert!(!explicit.allow_public_http);
        let enabled: NetworkDocument = serde_json::from_value(
            serde_json::json!({"origin":"http://media.example", "allow_public_http":true}),
        )
        .unwrap();
        assert!(enabled.allow_public_http);
        for value in [
            serde_json::json!(null),
            serde_json::json!("true"),
            serde_json::json!(1),
            serde_json::json!({}),
        ] {
            assert!(serde_json::from_value::<NetworkDocument>(
                serde_json::json!({"origin":"http://media.example", "allow_public_http":value})
            )
            .is_err());
        }
        assert!(serde_json::from_value::<NetworkDocument>(
            serde_json::json!({"origin":"http://media.example", "allow_http":true})
        )
        .is_err());
    }

    #[test]
    fn item_adapter_is_explicit_closed_and_defaults_to_direct_ts() {
        let base = serde_json::json!({"item_ref":"item-1","favorite_sha256":"a".repeat(64),"title":"Synthetic","provider":"m3u","url":"https://media.example/live"});
        let direct: ItemDocument = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(direct.input_adapter, ADAPTER);
        for (adapter, accepted) in [
            (ADAPTER, true),
            (HLS_ADAPTER, true),
            ("auto", false),
            ("hls", false),
            ("http-fmp4-v1", false),
        ] {
            let mut value = base.clone();
            value["input_adapter"] = adapter.into();
            let item = serde_json::from_value(value).unwrap();
            let mut candidate = document(
                "https://media.example/live",
                "https://media.example",
                vec![],
            );
            candidate.items = vec![item];
            assert_eq!(candidate.validate(&operator()).is_ok(), accepted);
        }
    }

    #[cfg(unix)]
    #[test]
    fn media_paths_require_private_output_and_trusted_absolute_tools() {
        use std::{
            fs::{self, DirBuilder, OpenOptions, Permissions},
            io::Write,
            os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        };

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let home = std::env::var_os("HOME").expect("test home");
        let scratch = PathBuf::from(home).join(format!(
            ".multiview-media-config-test-{}",
            Uuid::new_v4().simple()
        ));
        DirBuilder::new().mode(0o700).create(&scratch).unwrap();
        let _scratch = Scratch(scratch.clone());
        let output_root = scratch.join("output");
        DirBuilder::new().mode(0o700).create(&output_root).unwrap();
        let bin = scratch.join("bin");
        DirBuilder::new().mode(0o700).create(&bin).unwrap();
        let tools = ["ffmpeg", "ffprobe", "mkfifo"].map(|name| {
            let path = bin.join(name);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o755)
                .open(&path)
                .unwrap();
            file.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
            path
        });
        let document = OutputDocument {
            root: output_root.clone(),
            ffmpeg: tools[0].clone(),
            ffprobe: tools[1].clone(),
            mkfifo: tools[2].clone(),
        };
        assert!(validate_output_paths(&document).is_ok());
        // Prepare the actual plan, without executing these fixture tools.
        // Advertise exactly the item adapters, not every compiled adapter.
        for adapters in [vec![ADAPTER], vec![HLS_ADAPTER], vec![ADAPTER, HLS_ADAPTER]] {
            let mut candidate = self::document(
                "https://media.example/live",
                "https://media.example",
                vec![],
            );
            candidate.items.clear();
            for (index, adapter) in adapters.iter().enumerate() {
                let mut item = self::document(
                    "https://media.example/live",
                    "https://media.example",
                    vec![],
                )
                .items
                .remove(0);
                item.item_ref = format!("item-{index}");
                item.input_adapter = (*adapter).into();
                candidate.items.push(item);
            }
            candidate.output = OutputDocument {
                root: output_root.clone(),
                ffmpeg: tools[0].clone(),
                ffprobe: tools[1].clone(),
                mkfifo: tools[2].clone(),
            };
            let validated = candidate.validate(&operator()).unwrap();
            let mut plan = validated.prepare(&operator()).unwrap();
            let backend = plan.take_backend().unwrap();
            let expected: Vec<String> = adapters
                .iter()
                .map(|v| (*v).to_owned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            assert_eq!(backend.input_adapters, expected);
            assert_eq!(backend.profile.max_inputs, 9);
            assert_eq!(backend.media.max_disk_bytes, 128 * 1024 * 1024);
            assert!(plan
                .enrollment
                .as_ref()
                .unwrap()
                .items
                .iter()
                .all(|item| expected.contains(&item.input_adapter)));
        }

        fs::set_permissions(&output_root, Permissions::from_mode(0o755)).unwrap();
        assert!(validate_output_root(&output_root).is_err());
        fs::set_permissions(&output_root, Permissions::from_mode(0o700)).unwrap();

        fs::set_permissions(&tools[0], Permissions::from_mode(0o777)).unwrap();
        assert!(resolve_tool(&tools[0]).is_err());
        assert!(resolve_tool(Path::new("ffmpeg")).is_err());
    }
}

fn private_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => value.is_private(),
        IpAddr::V6(value) => {
            value.to_ipv4_mapped().is_none() && value.segments()[0] & 0xfe00 == 0xfc00
        }
    }
}

#[cfg(unix)]
fn validate_output_paths(output: &OutputDocument) -> anyhow::Result<()> {
    let public = std::env::current_dir()
        .and_then(|path| std::fs::canonicalize(path))
        .map_err(|_| invalid("cannot determine public directory"))?;
    let root = validate_output_root(&output.root)?;
    if root.starts_with(&public) {
        return Err(invalid("output root must be outside public directory"));
    }
    for tool in [&output.ffmpeg, &output.ffprobe, &output.mkfifo] {
        if resolve_tool(tool)?.starts_with(&public) {
            return Err(invalid("tool path must be outside public directory"));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_output_paths(_output: &OutputDocument) -> anyhow::Result<()> {
    Err(invalid("production media is unsupported on this platform"))
}

#[cfg(unix)]
fn validate_output_root(path: &Path) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid("output root must be a clean absolute path"));
    }
    let mut current = PathBuf::from("/");
    let uid = unsafe { libc::geteuid() };
    let components: Vec<_> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect();
    if components.is_empty() {
        return Err(invalid("output root must be a private directory"));
    }
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|_| invalid("output root is unavailable"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid("output root path is not protected"));
        }
        let last = index + 1 == components.len();
        if metadata.uid() != 0 && metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
            return Err(invalid("output root ancestry is writable by others"));
        }
        if last && (metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700) {
            return Err(invalid("output root must be owner-private"));
        }
    }
    Ok(current)
}

#[cfg(not(unix))]
fn validate_output_root(_path: &Path) -> anyhow::Result<PathBuf> {
    Err(invalid("production media is unsupported on this platform"))
}

#[cfg(unix)]
fn resolve_tool(path: &Path) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid("tool path must be absolute"));
    }
    // Resolve trusted package-manager symlinks once, then retain only the
    // canonical executable path. Runtime never searches PATH or re-resolves it.
    let canonical = std::fs::canonicalize(path).map_err(|_| invalid("tool is unavailable"))?;
    let metadata = std::fs::metadata(&canonical).map_err(|_| invalid("tool is unavailable"))?;
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != 0 && metadata.uid() != uid
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o022 != 0
    {
        return Err(invalid("tool executable is not trusted"));
    }
    let mut parent = canonical
        .parent()
        .ok_or_else(|| invalid("tool is unavailable"))?;
    loop {
        let row = std::fs::metadata(parent).map_err(|_| invalid("tool path is not trusted"))?;
        if !row.is_dir() || row.uid() != 0 && row.uid() != uid || row.mode() & 0o022 != 0 {
            return Err(invalid("tool path is not trusted"));
        }
        match parent.parent() {
            Some(next) if next != parent => parent = next,
            _ => break,
        }
    }
    Ok(canonical)
}

#[cfg(not(unix))]
fn resolve_tool(_path: &Path) -> anyhow::Result<PathBuf> {
    Err(invalid("production media is unsupported on this platform"))
}
