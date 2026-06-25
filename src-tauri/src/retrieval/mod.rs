//! Unified context-retrieval pipeline.
//!
//! Blends the three existing retrieval sources (repo-map symbols, claude-mem
//! chroma memory, and recently-edited project files) into a single reranked
//! top-k result set. See [`pipeline`] for the core logic and the `retrieve`
//! Tauri command in `commands::retrieve` for the invoke surface.

pub mod pipeline;

pub use pipeline::{
    apply_rank_order, build_rerank_prompt, parse_rank_order, retrieve_blended, RetrievalHit,
};
