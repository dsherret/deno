use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use deno_ast::ModuleSpecifier;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_runtime::deno_fs::FileSystem;
use deno_runtime::deno_node::NodePermissions;
use deno_runtime::deno_node::NodeResolutionMode;
use deno_runtime::deno_node::NpmResolver;
use deno_runtime::deno_node::PackageJson;
use deno_semver::package::PackageReq;

use crate::args::package_json::get_local_package_json_version_reqs;
use crate::util::path::specifier_to_file_path;

use super::common::types_package_name;
use super::CliNpmResolver;
use super::InnerCliNpmResolverRef;

pub struct CliNpmResolverByonmCreateOptions {
  pub fs: Arc<dyn FileSystem>,
  pub root_node_modules_dir: PathBuf,
}

pub fn create_byonm_npm_resolver(
  options: CliNpmResolverByonmCreateOptions,
) -> Arc<dyn CliNpmResolver> {
  Arc::new(ByonmCliNpmResolver {
    fs: options.fs,
    root_node_modules_dir: options.root_node_modules_dir,
  })
}

#[derive(Debug)]
pub struct ByonmCliNpmResolver {
  fs: Arc<dyn FileSystem>,
  root_node_modules_dir: PathBuf,
}

impl NpmResolver for ByonmCliNpmResolver {
  fn resolve_package_folder_from_package(
    &self,
    name: &str,
    referrer: &ModuleSpecifier,
    mode: NodeResolutionMode,
  ) -> Result<PathBuf, AnyError> {
    let package_root_path =
      self.resolve_package_folder_from_path(&referrer)?.unwrap(); // todo: don't unwrap
    let mut current_folder = package_root_path.as_path();
    loop {
      let node_modules_folder = if current_folder.ends_with("node_modules") {
        Cow::Borrowed(current_folder)
      } else {
        Cow::Owned(current_folder.join("node_modules"))
      };
      let sub_dir = join_package_name(&node_modules_folder, name);
      if self.fs.is_dir_sync(&sub_dir) {
        // if doing types resolution, only resolve the package if it specifies a types property
        if mode.is_types() && !name.starts_with("@types/") {
          let package_json = PackageJson::load_skip_read_permission(
            &*self.fs,
            sub_dir.join("package.json"),
          )?;
          if package_json.types.is_some() {
            return Ok(sub_dir);
          }
        } else {
          return Ok(sub_dir);
        }
      }

      // if doing type resolution, check for the existence of a @types package
      if mode.is_types() && !name.starts_with("@types/") {
        let sub_dir =
          join_package_name(&node_modules_folder, &types_package_name(name));
        if self.fs.is_dir_sync(&sub_dir) {
          return Ok(sub_dir);
        }
      }

      if let Some(parent) = current_folder.parent() {
        current_folder = parent;
      } else {
        break;
      }
    }

    bail!(
      "could not find package '{}' from referrer '{}'.",
      name,
      referrer
    );
  }

  fn resolve_package_folder_from_path(
    &self,
    specifier: &deno_core::ModuleSpecifier,
  ) -> Result<Option<PathBuf>, AnyError> {
    // todo: canonicalize?
    let path = specifier.to_file_path().unwrap();
    if self.in_npm_package(specifier) {
      let mut path = path.as_path();
      while let Some(parent) = path.parent() {
        if parent.file_name().and_then(|f| f.to_str()) == Some("node_modules") {
          return Ok(Some(path.to_path_buf()));
        } else {
          path = parent;
        }
      }
    } else {
      // find the folder with a package.json
      // todo(dsherret): not exactly correct, but good enough for a first pass
      let mut path = path.as_path();
      while let Some(parent) = path.parent() {
        if parent.join("package.json").exists() {
          return Ok(Some(parent.to_path_buf()));
        } else {
          path = parent;
        }
      }
    }
    Ok(None)
  }

  fn in_npm_package(&self, specifier: &ModuleSpecifier) -> bool {
    specifier.scheme() == "file" && specifier.path().contains("/node_modules/")
  }

  fn ensure_read_permission(
    &self,
    permissions: &dyn NodePermissions,
    path: &Path,
  ) -> Result<(), AnyError> {
    // todo: this
    Ok(())
  }
}

impl CliNpmResolver for ByonmCliNpmResolver {
  fn into_npm_resolver(self: Arc<Self>) -> Arc<dyn NpmResolver> {
    self
  }

  fn clone_snapshotted(&self) -> Arc<dyn CliNpmResolver> {
    todo!()
  }

  fn as_inner(&self) -> InnerCliNpmResolverRef {
    InnerCliNpmResolverRef::Byonm(self)
  }

  fn root_node_modules_path(&self) -> Option<std::path::PathBuf> {
    todo!()
  }

  fn resolve_pkg_folder_from_specifier(
    &self,
    specifier: &ModuleSpecifier,
  ) -> Result<Option<PathBuf>, AnyError> {
    todo!()
  }

  fn resolve_pkg_folder_from_deno_module_req(
    &self,
    req: &PackageReq,
    referrer: &ModuleSpecifier,
  ) -> Result<PathBuf, AnyError> {
    fn resolve_from_package_json(
      req: &PackageReq,
      fs: &dyn FileSystem,
      path: PathBuf,
    ) -> Result<PathBuf, AnyError> {
      let package_json = PackageJson::load_skip_read_permission(fs, path)?;
      let deps = get_local_package_json_version_reqs(&package_json);
      for (key, value) in deps {
        if let Ok(value) = value {
          if value.name == req.name
            && value.version_req.intersects(&req.version_req)
          {
            let package_path = package_json
              .path
              .parent()
              .unwrap()
              .join("node_modules")
              .join(&key);
            return Ok(package_path);
          }
        }
      }
      bail!(
        concat!(
          "Could not find a matching package for 'npm:{}' in '{}'. ",
          "You must specify this as a package.json dependency when the ",
          "node_modules folder is not managed by Deno.",
        ),
        req,
        package_json.path.display()
      );
    }

    // attempt to resolve the npm specifier from the referrer's package.json,
    // but otherwise fallback to the project's package.json
    if let Ok(file_path) = specifier_to_file_path(referrer) {
      let mut current_path = file_path.as_path();
      while let Some(dir_path) = current_path.parent() {
        let package_json_path = dir_path.join("package.json");
        if self.fs.exists_sync(&package_json_path) {
          return resolve_from_package_json(
            req,
            self.fs.as_ref(),
            package_json_path,
          );
        }
        current_path = dir_path;
      }
    }

    resolve_from_package_json(
      req,
      self.fs.as_ref(),
      self
        .root_node_modules_dir
        .parent()
        .unwrap()
        .join("package.json"),
    )
  }

  fn get_npm_process_state(&self) -> String {
    todo!()
  }

  fn check_state_hash(&self) -> Option<u64> {
    // it is very difficult to determine the check state hash for byonm
    // so we just return None to signify check caching is not supported
    None
  }
}

fn join_package_name(path: &Path, package_name: &str) -> PathBuf {
  let mut path = path.to_path_buf();
  // ensure backslashes are used on windows
  for part in package_name.split('/') {
    path = path.join(part);
  }
  path
}