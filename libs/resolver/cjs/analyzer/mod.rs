// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;

use deno_error::JsErrorBox;
use deno_maybe_sync::MaybeSend;
use deno_maybe_sync::MaybeSync;
use deno_media_type::MediaType;
use node_resolver::analyze::CjsAnalysis as ExtNodeCjsAnalysis;
use node_resolver::analyze::CjsAnalysisExports;
use node_resolver::analyze::CjsCodeAnalyzer;
use node_resolver::analyze::EsmAnalysisMode;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

use super::CjsTrackerRc;
use crate::npm::DenoInNpmPackageChecker;

#[cfg(feature = "deno_ast")]
mod deno_ast;

#[cfg(feature = "deno_ast")]
pub use deno_ast::DenoAstModuleExportAnalyzer;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModuleExportsAndReExports {
  pub exports: Vec<String>,
  pub reexports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DenoCjsAnalysis {
  /// The module was found to be an ES module.
  Esm,
  /// The module was found to be an ES module and
  /// it was analyzed for imports and exports.
  EsmAnalysis(ModuleExportsAndReExports),
  /// The module was CJS.
  Cjs(ModuleExportsAndReExports),
}

impl DenoCjsAnalysis {
  pub fn is_script(&self) -> bool {
    match self {
      DenoCjsAnalysis::Esm | DenoCjsAnalysis::EsmAnalysis(_) => true,
      DenoCjsAnalysis::Cjs(_) => false,
    }
  }
}

#[derive(Debug, Copy, Clone)]
pub struct NodeAnalysisCacheSourceHash(pub u64);

#[allow(clippy::disallowed_types)]
pub type NodeAnalysisCacheRc = deno_maybe_sync::MaybeArc<dyn NodeAnalysisCache>;

pub trait NodeAnalysisCache: MaybeSend + MaybeSync {
  fn compute_source_hash(&self, source: &str) -> NodeAnalysisCacheSourceHash;
  fn get_cjs_analysis(
    &self,
    specifier: &Url,
    source_hash: NodeAnalysisCacheSourceHash,
  ) -> Option<DenoCjsAnalysis>;
  fn set_cjs_analysis(
    &self,
    specifier: &Url,
    source_hash: NodeAnalysisCacheSourceHash,
    analysis: &DenoCjsAnalysis,
  );
}

pub struct NullNodeAnalysisCache;

impl NodeAnalysisCache for NullNodeAnalysisCache {
  fn compute_source_hash(&self, _source: &str) -> NodeAnalysisCacheSourceHash {
    NodeAnalysisCacheSourceHash(0)
  }

  fn get_cjs_analysis(
    &self,
    _specifier: &Url,
    _source_hash: NodeAnalysisCacheSourceHash,
  ) -> Option<DenoCjsAnalysis> {
    None
  }

  fn set_cjs_analysis(
    &self,
    _specifier: &Url,
    _source_hash: NodeAnalysisCacheSourceHash,
    _analysis: &DenoCjsAnalysis,
  ) {
  }
}

#[sys_traits::auto_impl]
pub trait DenoCjsCodeAnalyzerSys:
  sys_traits::FsRead + sys_traits::FsMetadata + MaybeSend + MaybeSync + 'static
{
}

#[allow(clippy::disallowed_types)]
pub type ModuleExportAnalyzerRc =
  deno_maybe_sync::MaybeArc<dyn ModuleExportAnalyzer>;

#[allow(clippy::disallowed_types)]
type ArcStr = std::sync::Arc<str>;

pub trait ModuleExportAnalyzer: MaybeSend + MaybeSync {
  fn analyze_esm_exports(
    &self,
    specifier: Url,
    media_type: MediaType,
    source: ArcStr,
  ) -> Result<ModuleExportsAndReExports, JsErrorBox>;
}

/// A module export analyzer that will error when parsing a module.
pub struct NotImplementedModuleExportAnalyzer;

impl ModuleExportAnalyzer for NotImplementedModuleExportAnalyzer {
  fn analyze_esm_exports(
    &self,
    _specifier: Url,
    _media_type: MediaType,
    _source: ArcStr,
  ) -> Result<ModuleExportsAndReExports, JsErrorBox> {
    debug_assert!(
      false,
      "Enable the deno_ast feature to get module export analysis."
    );
    // don't bother returning this for DenoRT when it doesn't exist.
    // Node.js doesn't include the exports: https://github.com/nodejs/merve/issues/26
    Ok(Default::default())
  }
}

#[allow(clippy::disallowed_types)]
pub type DenoCjsCodeAnalyzerRc<TSys> =
  deno_maybe_sync::MaybeArc<DenoCjsCodeAnalyzer<TSys>>;

pub struct DenoCjsCodeAnalyzer<TSys: DenoCjsCodeAnalyzerSys> {
  cache: NodeAnalysisCacheRc,
  cjs_tracker: CjsTrackerRc<DenoInNpmPackageChecker, TSys>,
  module_export_analyzer: ModuleExportAnalyzerRc,
  sys: TSys,
}

impl<TSys: DenoCjsCodeAnalyzerSys> DenoCjsCodeAnalyzer<TSys> {
  pub fn new(
    cache: NodeAnalysisCacheRc,
    cjs_tracker: CjsTrackerRc<DenoInNpmPackageChecker, TSys>,
    module_export_analyzer: ModuleExportAnalyzerRc,
    sys: TSys,
  ) -> Self {
    Self {
      cache,
      cjs_tracker,
      module_export_analyzer,
      sys,
    }
  }

  async fn inner_cjs_analysis(
    &self,
    specifier: &Url,
    source: &str,
    esm_analysis_mode: EsmAnalysisMode,
  ) -> Result<DenoCjsAnalysis, JsErrorBox> {
    let media_type = MediaType::from_specifier(specifier);
    if media_type == MediaType::Json {
      return Ok(DenoCjsAnalysis::Cjs(Default::default()));
    }

    let source = source.strip_prefix('\u{FEFF}').unwrap_or(source); // strip BOM
    let source_hash = self.cache.compute_source_hash(source);
    if let Some(analysis) = self.cache.get_cjs_analysis(specifier, source_hash)
    {
      match &analysis {
        DenoCjsAnalysis::Esm => match esm_analysis_mode {
          EsmAnalysisMode::SourceOnly => return Ok(analysis),
          EsmAnalysisMode::SourceImportsAndExports => {}
        },
        DenoCjsAnalysis::EsmAnalysis(_) | DenoCjsAnalysis::Cjs(_) => {
          return Ok(analysis);
        }
      }
    }

    let cjs_tracker = self.cjs_tracker.clone();
    let is_maybe_cjs = cjs_tracker
      .is_maybe_cjs(specifier, media_type)
      .map_err(JsErrorBox::from_err)?;
    let analysis = if is_maybe_cjs
      || esm_analysis_mode == EsmAnalysisMode::SourceImportsAndExports
    {
      let module_export_analyzer = self.module_export_analyzer.clone();

      let analyze = {
        let specifier = specifier.clone();
        let source: ArcStr = source.into();
        move || -> Result<_, JsErrorBox> {
          let analysis = if is_maybe_cjs {
            match merve::parse_commonjs(source.as_ref()) {
              Ok(result) => DenoCjsAnalysis::Cjs(ModuleExportsAndReExports {
                exports: result.exports().map(|e| e.name.to_string()).collect(),
                reexports: result
                  .reexports()
                  .map(|e| e.name.to_string())
                  .collect(),
              }),
              Err(err) => match err {
                merve::LexerError::EmptySource => {
                  // always just return this as cjs
                  DenoCjsAnalysis::Cjs(ModuleExportsAndReExports::default())
                }
                merve::LexerError::UnexpectedEsmImportMeta
                | merve::LexerError::UnexpectedEsmImport
                | merve::LexerError::UnexpectedEsmExport => {
                  DenoCjsAnalysis::Esm
                }
                merve::LexerError::UnexpectedParen
                | merve::LexerError::UnexpectedBrace
                | merve::LexerError::UnterminatedParen
                | merve::LexerError::UnterminatedBrace
                | merve::LexerError::UnterminatedTemplateString
                | merve::LexerError::UnterminatedStringLiteral
                | merve::LexerError::UnterminatedRegexCharacterClass
                | merve::LexerError::UnterminatedRegex
                | merve::LexerError::TemplateNestOverflow
                | merve::LexerError::Unknown(_) => {
                  // TODO(@dsherret): possibly return the error here
                  DenoCjsAnalysis::Cjs(ModuleExportsAndReExports::default())
                }
              },
            }
          } else {
            DenoCjsAnalysis::Esm
          };
          if is_maybe_cjs {
            cjs_tracker.set_is_known_script(&specifier, analysis.is_script());
          }
          match &analysis {
            DenoCjsAnalysis::Esm => match esm_analysis_mode {
              EsmAnalysisMode::SourceOnly => Ok(analysis),
              EsmAnalysisMode::SourceImportsAndExports => {
                let analysis = module_export_analyzer
                  .analyze_esm_exports(specifier, media_type, source)?;
                Ok(DenoCjsAnalysis::EsmAnalysis(analysis))
              }
            },
            DenoCjsAnalysis::EsmAnalysis(_) | DenoCjsAnalysis::Cjs(_) => {
              Ok(analysis)
            }
          }
        }
      };

      #[cfg(feature = "sync")]
      {
        crate::rt::spawn_blocking(analyze).await.unwrap()?
      }
      #[cfg(not(feature = "sync"))]
      analyze()?
    } else {
      DenoCjsAnalysis::Esm
    };

    self
      .cache
      .set_cjs_analysis(specifier, source_hash, &analysis);

    Ok(analysis)
  }
}

#[async_trait::async_trait(?Send)]
impl<TSys: DenoCjsCodeAnalyzerSys> CjsCodeAnalyzer
  for DenoCjsCodeAnalyzer<TSys>
{
  async fn analyze_cjs<'a>(
    &self,
    specifier: &Url,
    source: Option<Cow<'a, str>>,
    esm_analysis_mode: EsmAnalysisMode,
  ) -> Result<ExtNodeCjsAnalysis<'a>, JsErrorBox> {
    let source = match source {
      Some(source) => source,
      None => {
        if let Ok(path) = deno_path_util::url_to_file_path(specifier) {
          if let Ok(source_from_file) = self.sys.fs_read_to_string_lossy(path) {
            source_from_file
          } else {
            return Ok(ExtNodeCjsAnalysis::Cjs(CjsAnalysisExports {
              exports: vec![],
              reexports: vec![],
            }));
          }
        } else {
          return Ok(ExtNodeCjsAnalysis::Cjs(CjsAnalysisExports {
            exports: vec![],
            reexports: vec![],
          }));
        }
      }
    };
    let analysis = self
      .inner_cjs_analysis(specifier, &source, esm_analysis_mode)
      .await?;
    match analysis {
      DenoCjsAnalysis::Esm => Ok(ExtNodeCjsAnalysis::Esm(source, None)),
      DenoCjsAnalysis::EsmAnalysis(analysis) => Ok(ExtNodeCjsAnalysis::Esm(
        source,
        Some(CjsAnalysisExports {
          exports: analysis.exports,
          reexports: analysis.reexports,
        }),
      )),
      DenoCjsAnalysis::Cjs(analysis) => {
        Ok(ExtNodeCjsAnalysis::Cjs(CjsAnalysisExports {
          exports: analysis.exports,
          reexports: analysis.reexports,
        }))
      }
    }
  }
}
