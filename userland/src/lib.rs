//! Shared code for the ruOS userland binaries.
//!
//! - `proto`: the line-delimited JSON protocol between `ai` and `ruosd`.
//! - `config`: how `ruosd` finds its settings (file, QEMU fw_cfg, environment).
//! - `provider`: model backends (Claude API, offline rule-based fallback).
//! - `tools`: the system tools the agent may call, and their access policy.
//! - `agent`: the tool-use loop that ties a provider to the tools.

pub mod agent;
pub mod config;
pub mod proto;
pub mod provider;
pub mod tools;
