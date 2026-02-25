// Copyright 2018-2026 the Deno authors. MIT license.

use deno_ast::MediaType;
use deno_ast::ParsedSource;
use deno_error::JsErrorBox;
use deno_graph::ast::ParsedSourceStore;
use url::Url;

use super::ModuleExportAnalyzer;
use crate::cache::ParsedSourceCacheRc;

pub struct DenoAstModuleExportAnalyzer {
  parsed_source_cache: ParsedSourceCacheRc,
}

impl DenoAstModuleExportAnalyzer {
  pub fn new(parsed_source_cache: ParsedSourceCacheRc) -> Self {
    Self {
      parsed_source_cache,
    }
  }
}

#[allow(clippy::disallowed_types)]
type ArcStr = std::sync::Arc<str>;

impl ModuleExportAnalyzer for DenoAstModuleExportAnalyzer {
  fn analyze_esm_exports(
    &self,
    specifier: Url,
    media_type: MediaType,
    source: ArcStr,
  ) -> Result<ModuleExportsAndReExports, JsErrorBox> {
    let maybe_parsed_source =
      self.parsed_source_cache.remove_parsed_source(&specifier);
    let parsed_source = maybe_parsed_source
      .map(Ok)
      .unwrap_or_else(|| {
        deno_ast::parse_program(deno_ast::ParseParams {
          specifier,
          text: source,
          media_type,
          capture_tokens: true,
          scope_analysis: false,
          maybe_syntax: None,
        })
      })
      .map_err(JsErrorBox::from_err)?;
    let analysis = parsed_source.analyze_es_runtime_exports();
    Ok(super::ModuleExportsAndReExports {
      exports: analysis.exports,
      reexports: analysis.reexports,
    })
  }
}
