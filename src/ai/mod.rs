//! Background LLM helpers for inferring API workflow scenarios from OpenAPI.

mod provider;
mod workflows;

pub use crate::features::WorkflowManifest;
pub use provider::{
    llm_available, llm_unavailable_hint, ollama_reachable, print_llm_check, resolve_ai_config,
    resolve_ollama_model, AiConfig, AiProvider, AiResolution, ResolvedProvider,
};
pub use workflows::{
    generate_workflows_from_openapi, refine_workflows_from_openapi, save_workflow_manifest,
};
