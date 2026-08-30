use std::net::IpAddr;

use anyhow::{anyhow, Result};
use url::Url;

const MAX_ENDPOINT_BYTES: usize = 2 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedHttpEndpoint {
    base: Url,
    exact_loopback_ip: bool,
}

impl ValidatedHttpEndpoint {
    pub(crate) fn parse(endpoint: &str) -> Result<Self> {
        if endpoint.is_empty() || endpoint.len() > MAX_ENDPOINT_BYTES || endpoint.trim() != endpoint
        {
            return Err(anyhow!("semantic embedding endpoint is invalid"));
        }
        let raw_host = raw_url_host(endpoint)?;
        let mut base =
            Url::parse(endpoint).map_err(|_| anyhow!("semantic embedding endpoint is invalid"))?;
        if base.cannot_be_a_base() || base.host().is_none() {
            return Err(anyhow!("semantic embedding endpoint must contain a host"));
        }
        if authority_contains_credentials(endpoint)
            || !base.username().is_empty()
            || base.password().is_some()
        {
            return Err(anyhow!(
                "semantic embedding endpoint must not contain credentials"
            ));
        }
        if base.query().is_some() {
            return Err(anyhow!(
                "semantic embedding endpoint must not contain a query"
            ));
        }
        if base.fragment().is_some() {
            return Err(anyhow!(
                "semantic embedding endpoint must not contain a fragment"
            ));
        }

        let exact_loopback_ip = raw_host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
        match base.scheme() {
            "http" if !exact_loopback_ip => {
                return Err(anyhow!(
                    "plain HTTP semantic embedding requires an exact loopback IP host"
                ));
            }
            "http" | "https" => {}
            _ => {
                return Err(anyhow!(
                    "semantic embedding endpoint must use HTTPS or loopback HTTP"
                ));
            }
        }

        if !base.path().ends_with('/') {
            let normalized = format!("{}/", base.as_str());
            base = Url::parse(&normalized)
                .map_err(|_| anyhow!("semantic embedding endpoint is invalid"))?;
        }
        Ok(Self {
            base,
            exact_loopback_ip,
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        self.base.as_str()
    }

    pub(crate) const fn is_loopback(&self) -> bool {
        self.exact_loopback_ip
    }

    pub(super) fn route(&self, route: &str) -> Url {
        self.base
            .join(route)
            .expect("validated semantic embedding base URL accepts relative routes")
    }
}

fn authority_contains_credentials(endpoint: &str) -> bool {
    endpoint
        .split_once("://")
        .and_then(|(_, remainder)| remainder.split(['/', '?', '#']).next())
        .is_some_and(|authority| authority.contains('@'))
}

fn raw_url_host(endpoint: &str) -> Result<&str> {
    let authority = endpoint
        .split_once("://")
        .and_then(|(_, remainder)| remainder.split(['/', '?', '#']).next())
        .ok_or_else(|| anyhow!("semantic embedding endpoint is invalid"))?;
    if authority.contains('@') {
        return Err(anyhow!(
            "semantic embedding endpoint must not contain credentials"
        ));
    }
    if let Some(ipv6) = authority.strip_prefix('[') {
        return ipv6
            .split_once(']')
            .map(|(host, _)| host)
            .filter(|host| !host.is_empty())
            .ok_or_else(|| anyhow!("semantic embedding endpoint is invalid"));
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(authority, |(host, _)| host);
    if host.is_empty() {
        return Err(anyhow!("semantic embedding endpoint is invalid"));
    }
    Ok(host)
}
