// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use std::collections::BTreeSet;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use deno_core::anyhow;
use deno_core::anyhow::Context;
use deno_core::futures::future::LocalBoxFuture;
use deno_core::futures::stream::FuturesUnordered;
use deno_core::futures::FutureExt;
use deno_core::futures::StreamExt;
use deno_core::ModuleSpecifier;
use once_cell::sync::Lazy;

use deno_core::error::AnyError;

use crate::package_json::load_pkg_json;
use crate::path::to_file_specifier;
use crate::resolution::NodeResolverRc;
use crate::NodeModuleKind;
use crate::NodeResolutionMode;
use crate::NpmResolverRc;
use crate::PathClean;

#[derive(Debug, Clone)]
pub enum CjsAnalysis {
  /// File was found to be an ES module and the translator should
  /// load the code as ESM.
  Esm(String),
  Cjs(CjsAnalysisExports),
}

#[derive(Debug, Clone)]
pub struct CjsAnalysisExports {
  pub exports: Vec<String>,
  pub reexports: Vec<String>,
}

/// Code analyzer for CJS and ESM files.
#[async_trait::async_trait(?Send)]
pub trait CjsCodeAnalyzer {
  /// Analyzes CommonJs code for exports and reexports, which is
  /// then used to determine the wrapper ESM module exports.
  ///
  /// Note that the source is provided by the caller when the caller
  /// already has it. If the source is needed by the implementation,
  /// then it can use the provided source, or otherwise load it if
  /// necessary.
  async fn analyze_cjs(
    &self,
    specifier: &ModuleSpecifier,
    maybe_source: Option<String>,
  ) -> Result<CjsAnalysis, AnyError>;
}

pub struct NodeCodeTranslator<TCjsCodeAnalyzer: CjsCodeAnalyzer> {
  cjs_code_analyzer: TCjsCodeAnalyzer,
  fs: deno_fs::FileSystemRc,
  node_resolver: NodeResolverRc,
  npm_resolver: NpmResolverRc,
}

impl<TCjsCodeAnalyzer: CjsCodeAnalyzer> NodeCodeTranslator<TCjsCodeAnalyzer> {
  pub fn new(
    cjs_code_analyzer: TCjsCodeAnalyzer,
    fs: deno_fs::FileSystemRc,
    node_resolver: NodeResolverRc,
    npm_resolver: NpmResolverRc,
  ) -> Self {
    Self {
      cjs_code_analyzer,
      fs,
      node_resolver,
      npm_resolver,
    }
  }

  /// Translates given CJS module into ESM. This function will perform static
  /// analysis on the file to find defined exports and reexports.
  ///
  /// For all discovered reexports the analysis will be performed recursively.
  ///
  /// If successful a source code for equivalent ES module is returned.
  pub async fn translate_cjs_to_esm(
    &self,
    entry_specifier: &ModuleSpecifier,
    source: Option<String>,
  ) -> Result<String, AnyError> {
    let mut temp_var_count = 0;

    let analysis = self
      .cjs_code_analyzer
      .analyze_cjs(entry_specifier, source)
      .await?;

    let analysis = match analysis {
      CjsAnalysis::Esm(source) => return Ok(source),
      CjsAnalysis::Cjs(analysis) => analysis,
    };

    let mut source = vec![
      r#"import {createRequire as __internalCreateRequire} from "node:module";
      const require = __internalCreateRequire(import.meta.url);"#
        .to_string(),
    ];

    // use a BTreeSet to make the output deterministic for v8's code cache
    let mut all_exports = analysis.exports.into_iter().collect::<BTreeSet<_>>();

    if !analysis.reexports.is_empty() {
      let mut errors = Vec::new();
      self
        .analyze_reexports(
          entry_specifier,
          analysis.reexports,
          &mut all_exports,
          &mut errors,
        )
        .await;

      // surface errors afterwards in a deterministic way
      if !errors.is_empty() {
        errors.sort_by_cached_key(|e| e.to_string());
        return Err(errors.remove(0));
      }
    }

    source.push(format!(
      "const mod = require(\"{}\");",
      entry_specifier
        .to_file_path()
        .unwrap()
        .to_str()
        .unwrap()
        .replace('\\', "\\\\")
        .replace('\'', "\\\'")
        .replace('\"', "\\\"")
    ));

    for export in &all_exports {
      if export.as_str() != "default" {
        add_export(
          &mut source,
          export,
          &format!("mod[\"{}\"]", escape_for_double_quote_string(export)),
          &mut temp_var_count,
        );
      }
    }

    source.push("export default mod;".to_string());

    let translated_source = source.join("\n");
    Ok(translated_source)
  }

  async fn analyze_reexports<'a>(
    &'a self,
    entry_specifier: &url::Url,
    reexports: Vec<String>,
    all_exports: &mut BTreeSet<String>,
    // this goes through the modules concurrently, so collect
    // the errors in order to be deterministic
    errors: &mut Vec<anyhow::Error>,
  ) {
    struct Analysis {
      reexport_specifier: url::Url,
      referrer: url::Url,
      analysis: CjsAnalysis,
    }

    type AnalysisFuture<'a> = LocalBoxFuture<'a, Result<Analysis, AnyError>>;

    let mut handled_reexports: HashSet<ModuleSpecifier> = HashSet::default();
    handled_reexports.insert(entry_specifier.clone());
    let mut analyze_futures: FuturesUnordered<AnalysisFuture<'a>> =
      FuturesUnordered::new();
    let cjs_code_analyzer = &self.cjs_code_analyzer;
    let mut handle_reexports =
      |referrer: url::Url,
       reexports: Vec<String>,
       analyze_futures: &mut FuturesUnordered<AnalysisFuture<'a>>,
       errors: &mut Vec<anyhow::Error>| {
        // 1. Resolve the re-exports and start a future to analyze each one
        for reexport in reexports {
          let result = self.node_resolver.resolve(
            &reexport,
            &referrer,
            /* referrer kind */ NodeModuleKind::Esm,
            NodeResolutionMode::Execution,
          );
          let reexport_specifier = match result {
            Ok(specifier) => specifier,
            Err(err) => {
              errors.push(err.into());
              continue;
            }
          };

          if !handled_reexports.insert(reexport_specifier.clone()) {
            continue;
          }

          let referrer = referrer.clone();
          let future = async move {
            let analysis = cjs_code_analyzer
              .analyze_cjs(&reexport_specifier, None)
              .await
              .with_context(|| {
                format!(
                  "Could not load '{}' ({}) referenced from {}",
                  reexport, reexport_specifier, referrer
                )
              })?;

            Ok(Analysis {
              reexport_specifier,
              referrer,
              analysis,
            })
          }
          .boxed_local();
          analyze_futures.push(future);
        }
      };

    handle_reexports(
      entry_specifier.clone(),
      reexports,
      &mut analyze_futures,
      errors,
    );

    while let Some(analysis_result) = analyze_futures.next().await {
      // 2. Look at the analysis result and resolve its exports and re-exports
      let Analysis {
        reexport_specifier,
        referrer,
        analysis,
      } = match analysis_result {
        Ok(analysis) => analysis,
        Err(err) => {
          errors.push(err);
          continue;
        }
      };
      match analysis {
        CjsAnalysis::Esm(_) => {
          // todo(dsherret): support this once supporting requiring ES modules
          errors.push(anyhow::anyhow!(
            "Cannot require ES module '{}' from '{}'",
            reexport_specifier,
            referrer,
          ));
        }
        CjsAnalysis::Cjs(analysis) => {
          if !analysis.reexports.is_empty() {
            handle_reexports(
              reexport_specifier.clone(),
              analysis.reexports,
              &mut analyze_futures,
              errors,
            );
          }

          all_exports.extend(
            analysis
              .exports
              .into_iter()
              .filter(|e| e.as_str() != "default"),
          );
        }
      }
    }
  }
}

static RESERVED_WORDS: Lazy<HashSet<&str>> = Lazy::new(|| {
  HashSet::from([
    "abstract",
    "arguments",
    "async",
    "await",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "eval",
    "export",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "function",
    "get",
    "goto",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "int",
    "interface",
    "let",
    "long",
    "mod",
    "native",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "set",
    "short",
    "static",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "volatile",
    "while",
    "with",
    "yield",
  ])
});

fn add_export(
  source: &mut Vec<String>,
  name: &str,
  initializer: &str,
  temp_var_count: &mut usize,
) {
  fn is_valid_var_decl(name: &str) -> bool {
    // it's ok to be super strict here
    if name.is_empty() {
      return false;
    }

    if let Some(first) = name.chars().next() {
      if !first.is_ascii_alphabetic() && first != '_' && first != '$' {
        return false;
      }
    }

    name
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
  }

  // TODO(bartlomieju): Node actually checks if a given export exists in `exports` object,
  // but it might not be necessary here since our analysis is more detailed?
  if RESERVED_WORDS.contains(name) || !is_valid_var_decl(name) {
    *temp_var_count += 1;
    // we can't create an identifier with a reserved word or invalid identifier name,
    // so assign it to a temporary variable that won't have a conflict, then re-export
    // it as a string
    source.push(format!(
      "const __deno_export_{temp_var_count}__ = {initializer};"
    ));
    source.push(format!(
      "export {{ __deno_export_{temp_var_count}__ as \"{}\" }};",
      escape_for_double_quote_string(name)
    ));
  } else {
    source.push(format!("export const {name} = {initializer};"));
  }
}

fn escape_for_double_quote_string(text: &str) -> String {
  text.replace('\\', "\\\\").replace('"', "\\\"")
}
#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_add_export() {
    let mut temp_var_count = 0;
    let mut source = vec![];

    let exports = vec!["static", "server", "app", "dashed-export", "3d"];
    for export in exports {
      add_export(&mut source, export, "init", &mut temp_var_count);
    }
    assert_eq!(
      source,
      vec![
        "const __deno_export_1__ = init;".to_string(),
        "export { __deno_export_1__ as \"static\" };".to_string(),
        "export const server = init;".to_string(),
        "export const app = init;".to_string(),
        "const __deno_export_2__ = init;".to_string(),
        "export { __deno_export_2__ as \"dashed-export\" };".to_string(),
        "const __deno_export_3__ = init;".to_string(),
        "export { __deno_export_3__ as \"3d\" };".to_string(),
      ]
    )
  }
}
