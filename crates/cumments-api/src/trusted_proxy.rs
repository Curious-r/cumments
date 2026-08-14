//! Trusted reverse-proxy configuration: named presets and CIDR networks.
//!
//! The config file only accepts presets or CIDR strings; bare IPs must be
//! written with a `/32` or `/128` prefix so the operator's intent is
//! explicit. The resolved [`TrustedProxySet`] is used by `client_key` when
//! deciding whether an `X-Forwarded-For` header may be honored.

use anyhow::{Result, bail};
use ipnet::IpNet;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustedProxyPreset {
    Loopback,
    Private,
    LinkLocal,
}

impl TrustedProxyPreset {
    /// Human-readable list used in error messages.
    pub const NAMES: &'static [&'static str] = &["loopback", "private", "linklocal"];

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "loopback" => Some(Self::Loopback),
            "private" => Some(Self::Private),
            "linklocal" => Some(Self::LinkLocal),
            _ => None,
        }
    }

    pub fn networks(self) -> Vec<IpNet> {
        match self {
            Self::Loopback => vec![
                "127.0.0.0/8".parse().expect("built-in loopback network"),
                "::1/128".parse().expect("built-in loopback network"),
            ],
            Self::Private => vec![
                "10.0.0.0/8".parse().expect("built-in private network"),
                "172.16.0.0/12".parse().expect("built-in private network"),
                "192.168.0.0/16".parse().expect("built-in private network"),
                "fc00::/7".parse().expect("built-in private network"),
            ],
            Self::LinkLocal => vec![
                "169.254.0.0/16"
                    .parse()
                    .expect("built-in link-local network"),
                "fe80::/10".parse().expect("built-in link-local network"),
            ],
        }
    }
}

impl fmt::Display for TrustedProxyPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Loopback => "loopback",
            Self::Private => "private",
            Self::LinkLocal => "linklocal",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone)]
pub enum TrustedProxyRule {
    Preset(TrustedProxyPreset),
    Cidr(IpNet),
}

impl TrustedProxyRule {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if let Some(preset) = TrustedProxyPreset::parse(raw) {
            return Ok(Self::Preset(preset));
        }
        if let Ok(net) = raw.parse::<IpNet>() {
            return Ok(Self::Cidr(net));
        }
        if let Ok(ip) = raw.parse::<IpAddr>() {
            let prefix = if ip.is_ipv4() { "32" } else { "128" };
            return Err(format!(
                "`{raw}` is an IP address, not a CIDR; use `{raw}/{prefix}` \
                 or a named preset"
            ));
        }
        Err(format!(
            "`{raw}` is not a named preset ({}) or a CIDR (e.g. \"10.0.0.0/8\")",
            TrustedProxyPreset::NAMES.join(" | ")
        ))
    }
}

impl<'de> Deserialize<'de> for TrustedProxyRule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(de::Error::custom)
    }
}

/// A typed list of [`TrustedProxyRule`]s with per-entry error messages.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxyRules {
    rules: Vec<TrustedProxyRule>,
}

impl TrustedProxyRules {
    pub fn as_slice(&self) -> &[TrustedProxyRule] {
        &self.rules
    }
}

impl<'de> Deserialize<'de> for TrustedProxyRules {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RulesVisitor;

        impl<'de> Visitor<'de> for RulesVisitor {
            type Value = TrustedProxyRules;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an array of trusted proxy presets or CIDR networks")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut entries = Vec::new();
                while let Some(raw) = seq.next_element::<String>()? {
                    entries.push(raw);
                }
                let rules = parse_rule_entries(entries.iter().map(String::as_str))
                    .map_err(de::Error::custom)?;
                Ok(TrustedProxyRules { rules })
            }

            fn visit_str<E>(self, raw: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let entries = parse_env_entries(raw).map_err(de::Error::custom)?;
                let rules = parse_rule_entries(entries.iter().map(String::as_str))
                    .map_err(de::Error::custom)?;
                Ok(TrustedProxyRules { rules })
            }
        }

        deserializer.deserialize_any(RulesVisitor)
    }
}

fn parse_rule_entries<'a>(
    entries: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<TrustedProxyRule>, String> {
    entries
        .into_iter()
        .enumerate()
        .map(|(index, raw)| {
            TrustedProxyRule::parse(raw)
                .map_err(|message| format!("trusted_proxies[{index}]: {message}"))
        })
        .collect()
}

/// Environment variables arrive as a single string; accept both a
/// comma-separated list (`loopback,10.0.0.0/8`) and a JSON-style array
/// (`["loopback", "10.0.0.0/8"]`) so the env override stays convenient.
fn parse_env_entries(raw: &str) -> Result<Vec<String>, String> {
    let trimmed = raw.trim();
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        return serde_json::from_str(trimmed)
            .map_err(|error| format!("invalid trusted_proxies array: {error}"));
    }
    Ok(trimmed
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Resolved trusted networks ready for fast `contains` checks.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxySet {
    nets: Vec<IpNet>,
}

impl TrustedProxySet {
    /// Expands presets, validates explicit networks and deduplicates.
    ///
    /// `0.0.0.0/0` and `::/0` are rejected because they would make every
    /// peer a trusted proxy and therefore let any client forge its own
    /// rate-limit key via `X-Forwarded-For`.
    pub fn from_rules(rules: &[TrustedProxyRule]) -> Result<Self> {
        let mut nets = Vec::new();
        for rule in rules {
            match rule {
                TrustedProxyRule::Preset(preset) => {
                    for net in preset.networks() {
                        push_unique(&mut nets, net);
                    }
                }
                TrustedProxyRule::Cidr(net) => {
                    if net.prefix_len() == 0 {
                        bail!(
                            "`{net}` is not allowed: it would trust every peer; \
                             list only networks behind the reverse proxy"
                        );
                    }
                    push_unique(&mut nets, *net);
                }
            }
        }
        Ok(Self { nets })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = normalize_mapped_ipv4(ip);
        self.nets.iter().any(|net| net.contains(&ip))
    }
}

fn push_unique(nets: &mut Vec<IpNet>, net: IpNet) {
    if !nets.contains(&net) {
        nets.push(net);
    }
}

/// Treat IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) as their IPv4 form so
/// a dual-stack listener still matches `loopback` / `private` / IPv4 CIDRs.
fn normalize_mapped_ipv4(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        ip => ip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn rules(raw: &[&str]) -> Vec<TrustedProxyRule> {
        raw.iter()
            .map(|entry| TrustedProxyRule::parse(entry).expect("valid rule"))
            .collect()
    }

    #[test]
    fn parses_presets_and_cidrs() {
        let parsed = rules(&["loopback", "private", "10.42.0.0/16"]);
        assert_eq!(parsed.len(), 3);
        assert!(matches!(
            parsed[0],
            TrustedProxyRule::Preset(TrustedProxyPreset::Loopback)
        ));
        assert!(matches!(
            parsed[1],
            TrustedProxyRule::Preset(TrustedProxyPreset::Private)
        ));
        assert!(matches!(parsed[2], TrustedProxyRule::Cidr(_)));
    }

    #[test]
    fn rejects_bare_ips_with_fix_hint() {
        let error = TrustedProxyRule::parse("127.0.0.1").unwrap_err();
        assert!(error.contains("127.0.0.1/32"), "{error}");

        let error = TrustedProxyRule::parse("::1").unwrap_err();
        assert!(error.contains("::1/128"), "{error}");
    }

    #[test]
    fn rejects_unknown_entries() {
        let error = TrustedProxyRule::parse("k8s").unwrap_err();
        assert!(error.contains("loopback | private | linklocal"), "{error}");
    }

    #[test]
    fn rules_deserialize_from_comma_separated_env_value() {
        let rules: TrustedProxyRules =
            serde_json::from_str(r#""loopback,10.0.0.0/8""#).expect("comma-separated value");
        assert_eq!(rules.as_slice().len(), 2);
    }

    #[test]
    fn rules_deserialize_from_json_array_env_value() {
        let rules: TrustedProxyRules =
            serde_json::from_str(r#""[\"loopback\", \"10.0.0.0/8\"]""#).expect("JSON array string");
        assert_eq!(rules.as_slice().len(), 2);
    }

    #[test]
    fn preset_expansion_matches_expected_addresses() {
        let set = TrustedProxySet::from_rules(&rules(&["loopback", "private", "linklocal"]))
            .expect("valid presets");
        assert!(set.contains(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(set.contains(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(set.contains(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))));
        assert!(set.contains(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1))));
        assert!(set.contains(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1))));
        assert!(!set.contains(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn cidr_matches_inside_and_outside() {
        let set = TrustedProxySet::from_rules(&rules(&["10.0.0.0/8"])).expect("valid network");
        assert!(set.contains(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 1))));
        assert!(!set.contains(IpAddr::V4(Ipv4Addr::new(11, 0, 0, 1))));
    }

    #[test]
    fn v4_mapped_v6_matches_ipv4_networks() {
        let set = TrustedProxySet::from_rules(&rules(&["loopback"])).expect("valid preset");
        let mapped = IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001));
        assert!(set.contains(mapped));
    }

    #[test]
    fn rejects_wide_open_networks() {
        for entry in ["0.0.0.0/0", "::/0"] {
            let error = TrustedProxySet::from_rules(&rules(&[entry])).unwrap_err();
            assert!(
                error.to_string().contains("would trust every peer"),
                "{error}"
            );
        }
    }

    #[test]
    fn deduplicates_networks() {
        let set = TrustedProxySet::from_rules(&rules(&[
            "loopback",
            "127.0.0.0/8",
            "::1/128",
            "127.0.0.0/8",
        ]))
        .expect("valid rules");
        assert_eq!(set.nets.len(), 2);
    }
}
