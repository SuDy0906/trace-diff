//! Probe progress events for live TUI / verbose logs.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProgressEvent {
    Started {
        target: String,
    },
    TraceStarted {
        max_ttl: u8,
    },
    TraceHop {
        ttl: u8,
        max_ttl: u8,
        address: Option<String>,
        samples: u32,
    },
    TraceFinished {
        hops: usize,
        reached: bool,
    },
    TraceSkipped {
        reason: String,
    },
    L7Phase {
        phase: String,
    },
    L7Finished {
        total_ms: f64,
        status: Option<u16>,
    },
    Saving,
    Done,
    Error {
        message: String,
    },
}

impl ProgressEvent {
    pub fn hop(ttl: u8, max_ttl: u8, address: Option<IpAddr>, samples: u32) -> Self {
        Self::TraceHop {
            ttl,
            max_ttl,
            address: address.map(|a| a.to_string()),
            samples,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Started { target } => format!("starting probe → {target}"),
            Self::TraceStarted { max_ttl } => format!("traceroute (max TTL {max_ttl})"),
            Self::TraceHop {
                ttl,
                max_ttl,
                address,
                ..
            } => format!("hop {ttl}/{max_ttl}: {}", address.as_deref().unwrap_or("*")),
            Self::TraceFinished { hops, reached } => {
                format!("traceroute done ({hops} hops, reached={reached})")
            }
            Self::TraceSkipped { reason } => format!("traceroute skipped: {reason}"),
            Self::L7Phase { phase } => format!("L7: {phase}"),
            Self::L7Finished { total_ms, status } => {
                format!("L7 done ({total_ms:.1}ms, status={status:?})")
            }
            Self::Saving => "saving run…".into(),
            Self::Done => "complete".into(),
            Self::Error { message } => format!("error: {message}"),
        }
    }
}
