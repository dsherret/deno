// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use deno_core::error::AnyError;
use deno_core::futures::FutureExt;
use deno_core::resolve_url_or_path;
use deno_graph::Module;
use deno_runtime::colors;

use crate::args::BundleFlags;
use crate::args::CliOptions;
use crate::args::Flags;
use crate::args::PackFlags;
use crate::args::TsConfigType;
use crate::args::TypeCheckMode;
use crate::graph_util::error_for_any_npm_specifier;
use crate::proc_state::ProcState;
use crate::util;
use crate::util::display;
use crate::util::file_watcher::ResolutionResult;

pub async fn pack(flags: Flags, pack_flags: PackFlags) -> Result<(), AnyError> {
  let cli_options = Arc::new(CliOptions::from_flags(flags)?);

  log::info!(
    "{} \"deno pack\" is unstable and may be removed or drastically change in the future.",
    colors::yellow("Warning"),
  );

  let module_specifier =
    resolve_url_or_path(&pack_flags.source_file, cli_options.initial_cwd())?;

  let resolver = |_| {
    let cli_options = cli_options.clone();
    let module_specifier = &module_specifier;
    async move {
      log::debug!(">>>>> pack START");
      let ps = ProcState::from_cli_options(cli_options).await?;
      let graph = ps
        .module_graph_builder
        .create_graph_and_maybe_check(vec![module_specifier.clone()])
        .await?;

      let mut paths_to_watch: Vec<PathBuf> = graph
        .specifiers()
        .filter_map(|(_, r)| {
          r.ok().and_then(|module| match module {
            Module::Esm(m) => m.specifier.to_file_path().ok(),
            Module::Json(m) => m.specifier.to_file_path().ok(),
            // nothing to watch
            Module::Node(_) | Module::Npm(_) | Module::External(_) => None,
          })
        })
        .collect();

      if let Ok(Some(import_map_path)) = ps
        .options
        .resolve_import_map_specifier()
        .map(|ms| ms.and_then(|ref s| s.to_file_path().ok()))
      {
        paths_to_watch.push(import_map_path);
      }

      Ok((paths_to_watch, graph, ps))
    }
    .map(move |result| match result {
      Ok((paths_to_watch, graph, ps)) => ResolutionResult::Restart {
        paths_to_watch,
        result: Ok((ps, graph)),
      },
      Err(e) => ResolutionResult::Restart {
        paths_to_watch: vec![module_specifier.to_file_path().unwrap()],
        result: Err(e),
      },
    })
  };

  let operation = |(ps, graph): (ProcState, Arc<deno_graph::ModuleGraph>)| {
    let out_file = &pack_flags.out_file;
    async move {
      let pack_output =
        pack_module_graph(graph.as_ref(), &ps, pack_flags.lib).await?;
      log::debug!(">>>>> pack END");

      if let Some(out_file) = out_file {
        output_to_file(out_file, &pack_output.js)?;
        if let Some(dts) = &pack_output.dts {
          output_to_file(&out_file.with_extension("d.ts"), dts)?;
        }
      } else {
        println!("{}", pack_output.js);
      }

      Ok(())
    }
  };

  if cli_options.watch_paths().is_some() {
    util::file_watcher::watch_func(
      resolver,
      operation,
      util::file_watcher::PrintConfig {
        job_name: "Bundle".to_string(),
        clear_screen: !cli_options.no_clear_screen(),
      },
    )
    .await?;
  } else {
    let module_graph =
      if let ResolutionResult::Restart { result, .. } = resolver(None).await {
        result?
      } else {
        unreachable!();
      };
    operation(module_graph).await?;
  }

  Ok(())
}

fn output_to_file(out_file: &Path, text: &str) -> Result<(), AnyError> {
  let output_bytes = text.as_bytes();
  let output_len = output_bytes.len();
  util::fs::write_file(out_file, output_bytes, 0o644)?;
  log::info!(
    "{} {} ({})",
    colors::green("Emit"),
    out_file.display(),
    colors::gray(display::human_size(output_len as f64))
  );
  Ok(())
}

struct PackResult {
  js: String,
  dts: Option<String>,
}

async fn pack_module_graph(
  graph: &deno_graph::ModuleGraph,
  ps: &ProcState,
  lib: bool,
) -> Result<PackResult, AnyError> {
  log::info!("{} {}", colors::green("Pack"), graph.roots[0]);

  if lib {
    let dts_result = tokio::task::spawn_blocking({
      let ps = ps.clone();
      let graph = graph.clone(); // todo: don't clone
      move || {
        deno_emit::pack_dts(
          &graph,
          &ps.parsed_source_cache.as_capturing_parser(),
        )
      }
    });
    let js = deno_emit::pack(
      graph,
      &ps.parsed_source_cache.as_capturing_parser(),
      deno_emit::PackOptions {
        include_remote: !lib,
      },
    )?;
    let dts = dts_result.await.unwrap()?;
    Ok(PackResult { dts: Some(dts), js })
  } else {
    let js_source = deno_emit::pack(
      graph,
      &ps.parsed_source_cache.as_capturing_parser(),
      deno_emit::PackOptions {
        include_remote: !lib,
      },
    )?;
    Ok(PackResult {
      js: js_source,
      dts: None,
    })
  }
}
