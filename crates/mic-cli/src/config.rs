use std::collections::HashSet;
use std::net::IpAddr;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default, rename = "mic")]
    pub mics: Vec<MicConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MicConfig {
    pub id: String,
    pub kind: MicKind,
    pub address: IpAddr,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MicKind {
    ShureUlxd,
    ShureAxient,
    SennheiserEwdx,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let config: Config = toml::from_str(&text)
            .with_context(|| format!("parsing config file {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        for m in &self.mics {
            if !ids.insert(m.id.as_str()) {
                bail!("duplicate mic id '{}' in config", m.id);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[[mic]]
id = "ulxd-1"
kind = "shure-ulxd"
address = "10.0.0.30"
port = 2202

[[mic]]
id = "ewdx-1"
kind = "sennheiser-ewdx"
address = "10.0.0.31"
"#;

    #[test]
    fn parses_example_config() {
        let cfg: Config = toml::from_str(EXAMPLE).unwrap();
        assert_eq!(cfg.mics.len(), 2);
        assert_eq!(cfg.mics[0].kind, MicKind::ShureUlxd);
        assert_eq!(cfg.mics[0].port, Some(2202));
        assert_eq!(cfg.mics[1].kind, MicKind::SennheiserEwdx);
        assert_eq!(cfg.mics[1].port, None);
    }

    #[test]
    fn rejects_duplicate_ids() {
        let dup = EXAMPLE.replace("ewdx-1", "ulxd-1");
        let cfg: Config = toml::from_str(&dup).unwrap();
        assert!(cfg.validate().is_err());
    }
}
