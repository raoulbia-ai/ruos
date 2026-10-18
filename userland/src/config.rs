//! `ruosd` settings.
//!
//! Sources, later ones overriding earlier ones:
//! 1. `/etc/ruos/ruosd.conf` (baked into the image)
//! 2. QEMU fw_cfg blob `opt/ruos/config` (passed by the host at boot, so secrets
//!    such as the API key never enter the image)
//! 3. Process environment
//!
//! File format: `KEY=VALUE` lines, `#` comments, blank lines ignored.

use std::collections::HashMap;
use std::fs;

pub const CONF_FILE: &str = "/etc/ruos/ruosd.conf";
pub const FW_CFG_FILE: &str = "/sys/firmware/qemu_fw_cfg/by_name/opt/ruos/config/raw";

const KEYS: &[&str] = &[
    "RUOS_PROVIDER",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_BASE_URL",
    "RUOS_MODEL",
    "RUOS_EFFORT",
    "HTTPS_PROXY",
];

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderKind {
    Anthropic,
    Offline,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub provider: ProviderKind,
    pub api_key: Option<String>,
    pub base_url: String,
    pub model: String,
    pub effort: String,
    pub https_proxy: Option<String>,
}

pub fn parse(text: &str, into: &mut HashMap<String, String>) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"');
            into.insert(k.trim().to_string(), v.to_string());
        }
    }
}

impl Config {
    pub fn load() -> Config {
        let mut kv = HashMap::new();
        for path in [CONF_FILE, FW_CFG_FILE] {
            if let Ok(text) = fs::read_to_string(path) {
                parse(&text, &mut kv);
            }
        }
        for key in KEYS {
            if let Ok(v) = std::env::var(key) {
                kv.insert(key.to_string(), v);
            }
        }
        Config::from_map(&kv)
    }

    pub fn from_map(kv: &HashMap<String, String>) -> Config {
        let get = |k: &str| kv.get(k).filter(|v| !v.is_empty()).cloned();
        let api_key = get("ANTHROPIC_API_KEY");
        let provider = match get("RUOS_PROVIDER").as_deref() {
            Some("offline") => ProviderKind::Offline,
            Some("anthropic") => ProviderKind::Anthropic,
            // No explicit choice: use Claude when a key is available.
            _ if api_key.is_some() => ProviderKind::Anthropic,
            _ => ProviderKind::Offline,
        };
        Config {
            provider,
            api_key,
            base_url: get("ANTHROPIC_BASE_URL").unwrap_or_else(|| "https://api.anthropic.com".into()),
            model: get("RUOS_MODEL").unwrap_or_else(|| "claude-opus-5-5".into()),
            effort: get("RUOS_EFFORT").unwrap_or_else(|| "medium".into()),
            https_proxy: get("HTTPS_PROXY"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_defaults_follow_key() {
        let mut kv = HashMap::new();
        assert_eq!(Config::from_map(&kv).provider, ProviderKind::Offline);
        parse("# comment\nANTHROPIC_API_KEY=\"sk-test\"\n\nRUOS_EFFORT = low\n", &mut kv);
        let c = Config::from_map(&kv);
        assert_eq!(c.provider, ProviderKind::Anthropic);
        assert_eq!(c.api_key.as_deref(), Some("sk-test"));
        assert_eq!(c.effort, "low");
        kv.insert("RUOS_PROVIDER".into(), "offline".into());
        assert_eq!(Config::from_map(&kv).provider, ProviderKind::Offline);
    }
}
