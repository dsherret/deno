// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::path::Path;
use std::path::PathBuf;

use deno_ast::ModuleSpecifier;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::op;
use deno_core::Extension;
use deno_core::OpState;
use deno_runtime::permissions::Permissions;

use crate::npm::NpmPackageResolver;
use crate::proc_state::ProcState;

pub mod bench;
pub mod testing;

pub fn cli_exts(ps: ProcState) -> Vec<Extension> {
  vec![init_proc_state(ps)]
}

fn init_proc_state(ps: ProcState) -> Extension {
  let unstable = ps.options.unstable();
  Extension::builder()
    .state(move |state| {
      state.put(ps.clone());
      Ok(())
    })
    .middleware(move |op| {
      if !unstable {
        // only use the ops below for `--unstable`
        return op;
      }
      match op.name {
        "op_require_resolve_deno_dir" => op_require_resolve_deno_dir::decl(),
        "op_require_is_deno_dir_package" => {
          op_require_is_deno_dir_package::decl()
        }
        "op_require_read_file" => op_require_read_file::decl(),
        _ => op,
      }
    })
    .build()
}

#[op]
fn op_require_resolve_deno_dir(
  s: &mut OpState,
  request: String,
  parent_filename: String,
) -> Option<String> {
  let ps = s.borrow::<ProcState>();
  let referrer = ModuleSpecifier::from_file_path(parent_filename).ok()?;
  ps.npm_resolver
    .resolve_package_from_package(&request, &referrer)
    .ok()
    .map(|p| p.folder_path.to_string_lossy().to_string())
}

#[op]
fn op_require_is_deno_dir_package(s: &mut OpState, path: String) -> bool {
  let ps = s.borrow::<ProcState>();
  let specifier = match ModuleSpecifier::from_file_path(path) {
    Ok(p) => p,
    Err(_) => return false,
  };
  ps.npm_resolver.in_npm_package(&specifier)
}

#[op]
fn op_require_read_file(
  s: &mut OpState,
  file_path: String,
) -> Result<String, AnyError> {
  let file_path = PathBuf::from(file_path);
  ensure_require_read_permission(s, &file_path)?;
  let contents = std::fs::read_to_string(file_path)?;
  Ok(contents)
}

fn ensure_require_read_permission(
  s: &mut OpState,
  file_path: &Path,
) -> Result<(), AnyError> {
  let ps = s.borrow::<ProcState>();
  let specifier = match ModuleSpecifier::from_file_path(file_path) {
    Ok(p) => p,
    Err(()) => bail!("Invalid path: {}", file_path.display()),
  };
  // allow reading if it's in the deno_dir node modules
  if let Ok(pkg) = ps.npm_resolver.resolve_package_from_specifier(&specifier) {
    let canonicalized_root_folder = std::fs::canonicalize(pkg.folder_path)?;
    let canonicalized_file_path = std::fs::canonicalize(file_path)?;
    if canonicalized_file_path.starts_with(canonicalized_root_folder) {
      return Ok(());
    }
  }
  let permissions = s.borrow_mut::<Permissions>();
  permissions.read.check(file_path)?;
  Ok(())
}
