//! Test-only complete operator JSON shared by normal-binary and protected-loader regressions.
use serde_json::{json, Value};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
};
pub const TOKEN: &str = "production_fixture_control_owner_aaaaaaaaaaaaaaaa";
pub const EXPIRING_TOKEN: &str = "production_fixture_control_expiring_bbbbbbbbbbbb";
const SIGNING_KEY: &str = "production_fixture_signing_key_aaaaaaaaaaaaaaaa";
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProductionMode {
    RawMpegTs,
    LiveHlsTs,
}
impl ProductionMode {
    pub fn adapter(self) -> &'static str {
        match self {
            Self::RawMpegTs => "http-mpegts-v1",
            Self::LiveHlsTs => "http-hls-ts-v1",
        }
    }
    pub fn path(self, id: usize) -> String {
        match self {
            Self::RawMpegTs => format!("source-{id}.ts"),
            Self::LiveHlsTs => format!("source-{id}/master.m3u8"),
        }
    }
}

fn production_source_items(
    mode: ProductionMode,
    source_origin: &str,
    inputs: &[(u8, PathBuf, String, u64)],
) -> Vec<Value> {
    inputs.iter().enumerate().map(|(index, (_, _, sha, _))| json!({
        "item_ref": format!("production_item_{}", index + 1), "favorite_sha256":sha,
        "title":format!("Fixture source {}", index + 1), "provider":"fixture",
        "input_adapter":mode.adapter(), "url":format!("{source_origin}/{}", mode.path(index + 1)),
    })).collect()
}

pub struct Config<'a> {
    pub mode: ProductionMode,
    pub source_origin: &'a str,
    pub private_peer: IpAddr,
    pub inputs: &'a [(u8, PathBuf, String, u64)],
    pub api_origin: &'a str,
    pub expires: u64,
    pub short_expires: u64,
    pub output: &'a Path,
    pub ffmpeg: &'a str,
    pub ffprobe: &'a str,
    pub mkfifo: &'a str,
}
impl Config<'_> {
    pub fn document(&self) -> Value {
        let Self {
            mode,
            source_origin,
            private_peer,
            inputs,
            api_origin,
            expires,
            short_expires,
            output,
            ffmpeg,
            ffprobe,
            mkfifo,
        } = self;
        let items = production_source_items(*mode, source_origin, inputs);
        let allowed = json!([
            {"subject":"production_owner_A","installation_id":"production_install_A"},
            {"subject":"production_expiry_B","installation_id":"production_install_B"}
        ]);
        json!({
            "version":1,"server_id":"production_fixture_server","origin":api_origin,
            "listener":{"bind_ip":"127.0.0.1","https":true,"lan_approved":false},
            "allowed_origins":["https://native.example.test"],
            "signing_key":SIGNING_KEY,
            "credentials":[
                {"installation_id":"production_install_A","control_bearer":TOKEN,"subject":"production_owner_A","revision":1,"expires_at_ms":expires},
                {"installation_id":"production_install_B","control_bearer":EXPIRING_TOKEN,"subject":"production_expiry_B","revision":1,"expires_at_ms":short_expires}
            ],
            "media":{"version":1,"adapter":"http-mpegts-v1","source_ref":"production_source_A",
                "profile_uuid":"b1e52616-9af1-4fcb-937e-0e70e654c2f1","config_revision":"production_config_A","policy_revision":"production_policy_A",
                "allowed":allowed,
                "network":[{"origin":source_origin,"private_addresses":[private_peer.to_string()]}],
                "items":items,"output":{"root":output,"ffmpeg":ffmpeg,"ffprobe":ffprobe,"mkfifo":mkfifo},
                "max_duration_seconds":600}
        })
    }
}
