//! Closed diagnostic schema only. A marker never supplies media authority.
use std::{fmt, num::NonZeroU64};

pub(crate) const HEADER: &str = "x-private-media-attempt";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Marker(NonZeroU64);
impl Marker {
    pub(crate) fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        // One canonical decimal field: no signs, whitespace, leading zeroes or overflow.
        if value.is_empty() || value.len() > 20 || value.starts_with('0') {
            return None;
        }
        let mut result = 0u64;
        for byte in value.bytes() {
            if !byte.is_ascii_digit() {
                return None;
            }
            result = result
                .checked_mul(10)?
                .checked_add(u64::from(byte - b'0'))?;
        }
        Self::new(result)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Site {
    BodyFirst,
    BodyLater,
    Supervisor,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    TemporaryBusy,
    Unavailable,
    Denied,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transport {
    Http,
    Tls,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Terminal {
    pub(crate) marker: Marker,
    pub(crate) site: Site,
    pub(crate) kind: Kind,
    pub(crate) transport: Transport,
}
impl fmt::Display for Terminal {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let site = match self.site {
            Site::BodyFirst => "body_first",
            Site::BodyLater => "body_later",
            Site::Supervisor => "supervisor",
        };
        let kind = match self.kind {
            Kind::TemporaryBusy => "temporary_busy",
            Kind::Unavailable => "unavailable",
            Kind::Denied => "denied",
        };
        let transport = match self.transport {
            Transport::Http => "http",
            Transport::Tls => "tls",
        };
        write!(out, "private_media_attempt version=1 marker={} site={site} reason={kind} transport={transport} terminal=closed", self.marker.get())
    }
}
impl Terminal {
    #[cfg(test)]
    pub(crate) fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split(' ');
        if fields.next()? != "private_media_attempt" || fields.next()? != "version=1" {
            return None;
        }
        let marker = Marker::parse(fields.next()?.strip_prefix("marker=")?)?;
        let site = match fields.next()? {
            "site=body_first" => Site::BodyFirst,
            "site=body_later" => Site::BodyLater,
            "site=supervisor" => Site::Supervisor,
            _ => return None,
        };
        let kind = match fields.next()? {
            "reason=temporary_busy" => Kind::TemporaryBusy,
            "reason=unavailable" => Kind::Unavailable,
            "reason=denied" => Kind::Denied,
            _ => return None,
        };
        let transport = match fields.next()? {
            "transport=http" => Transport::Http,
            "transport=tls" => Transport::Tls,
            _ => return None,
        };
        if fields.next()? != "terminal=closed" || fields.next().is_some() {
            return None;
        }
        Some(Self {
            marker,
            site,
            kind,
            transport,
        })
    }
}
