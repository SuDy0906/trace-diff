//! Background LLM helpers for inferring API workflow scenarios from OpenAPI.

mod provider;
mod workflows;

pub use crate::features::WorkflowManifest;
pub use provider::{llm_available, resolve_ollama_model, AiConfig, AiProvider};
pub use workflows::{
    generate_workflows_from_openapi, refine_workflows_from_openapi, save_workflow_manifest,
};
