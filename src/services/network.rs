//! "Connect to Server": parsing what the user typed into a server URI, and
//! the list of recently used servers (kept in GSettings).

use gettextrs::gettext;
use gtk::prelude::*;

use crate::services::volumes::NETWORK_SCHEMES;
use crate::util::settings;

/// How many recent servers are remembered.
pub const MAX_RECENT_SERVERS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressError {
    Empty,
    /// No `scheme://` and not a Windows share path either.
    MissingScheme,
    UnknownScheme(String),
    MissingHost,
}

impl AddressError {
    pub fn message(&self) -> String {
        match self {
            Self::Empty => gettext("Enter a server address"),
            Self::MissingScheme => {
                gettext("Start the address with a protocol, e.g. “smb://” or “sftp://”")
            }
            Self::UnknownScheme(scheme) => {
                gettext("“{}://” isn't a supported protocol").replace("{}", scheme)
            }
            Self::MissingHost => gettext("Add a server name after the protocol"),
        }
    }
}

/// Turns user input into a server URI: `smb://nas/share`,
/// `sftp://me@host:2222/srv`… Windows share paths (`\\nas\share`) become
/// `smb://` URIs.
pub fn parse_address(text: &str) -> Result<String, AddressError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AddressError::Empty);
    }
    if let Some(unc) = text.strip_prefix(r"\\") {
        let path = unc.replace('\\', "/");
        if path.is_empty() || path.starts_with('/') {
            return Err(AddressError::MissingHost);
        }
        return Ok(format!("smb://{path}"));
    }
    let Some((scheme, rest)) = text.split_once("://") else {
        return Err(AddressError::MissingScheme);
    };
    let scheme = scheme.to_ascii_lowercase();
    if !NETWORK_SCHEMES.contains(&scheme.as_str()) {
        return Err(AddressError::UnknownScheme(scheme));
    }
    if rest.is_empty() || rest.starts_with('/') {
        return Err(AddressError::MissingHost);
    }
    Ok(format!("{scheme}://{rest}"))
}

/// `servers` with `uri` moved (or added) to the front, without
/// duplicates, at most [`MAX_RECENT_SERVERS`] long.
pub fn add_recent(servers: &[String], uri: &str) -> Vec<String> {
    let same = |s: &String| s.trim_end_matches('/') == uri.trim_end_matches('/');
    std::iter::once(uri.to_owned())
        .chain(servers.iter().filter(|s| !same(s)).cloned())
        .take(MAX_RECENT_SERVERS)
        .collect()
}

pub fn recent_servers() -> Vec<String> {
    settings()
        .strv("recent-servers")
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub fn remember_server(uri: &str) {
    let servers = add_recent(&recent_servers(), uri);
    let _ = settings().set_strv("recent-servers", servers);
}

pub fn forget_server(uri: &str) {
    let servers: Vec<String> = recent_servers().into_iter().filter(|s| s != uri).collect();
    let _ = settings().set_strv("recent-servers", servers);
}
