// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use deno_core::error::AnyError;
use deno_core::futures::future::BoxFuture;
use deno_core::futures::FutureExt;

use super::cache::NpmCache;
use super::resolution::NpmPackageResolution;

// todo:
// - Need to ensure only one process enters this at a time.

pub async fn setup_node_modules(
  packages: Vec<NpmPackageResolution>,
  cache: NpmCache,
  output_dir: PathBuf,
) -> Result<(), AnyError> {
  fs::create_dir_all(&output_dir)?;
  setup_packages(packages, cache, output_dir).await?;

  Ok(())
}

fn setup_packages(
  packages: Vec<NpmPackageResolution>,
  cache: NpmCache,
  output_dir: PathBuf,
) -> BoxFuture<'static, Result<(), AnyError>> {
  // boxed because it's recursive
  async move {
    // todo(dsherret): parallelize
    for package in packages {
      let cache_folder = cache
        .ensure_package(&package.id, &package.dist)
        .await?
        .join("package");
      let local_folder = output_dir.join(&package.id.name);
      remove_dir_all(&local_folder)?;
      if package.sub_packages.is_empty() {
        // no sub packages, so create a symlink
        symlink_dir(&cache_folder, &local_folder)?;
      } else {
        // there's sub packages, so symlink the children
        symlink_dir_children(&cache_folder, &local_folder)?;
        let sub_node_modules = local_folder.join("node_modules");
        fs::create_dir(&sub_node_modules)?;
        setup_packages(package.sub_packages, cache.clone(), sub_node_modules)
          .await?;
      }
    }
    Ok(())
  }
  .boxed()
}

fn remove_dir_all(path: &Path) -> Result<(), AnyError> {
  match fs::remove_dir_all(path) {
    Ok(_) => Ok(()),
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(err) => Err(err.into()),
  }
}

fn symlink_dir_children(
  oldpath: &Path,
  newpath: &Path,
) -> Result<(), AnyError> {
  debug_assert!(oldpath.is_dir());
  fs::create_dir(&newpath)?;
  for entry in fs::read_dir(oldpath)? {
    let entry = entry?;
    if entry.file_type()?.is_dir() {
      symlink_dir(&entry.path(), &newpath.join(entry.file_name()))?;
    } else {
      symlink_file(&entry.path(), &newpath.join(entry.file_name()))?;
    }
  }
  Ok(())
}

// todo(dsherret): try to consolidate these symlink_dir and symlink_file functions
fn symlink_dir(oldpath: &Path, newpath: &Path) -> Result<(), AnyError> {
  use std::io::Error;
  let err_mapper = |err: Error| {
    Error::new(
      err.kind(),
      format!(
        "{}, symlink '{}' -> '{}'",
        err,
        oldpath.display(),
        newpath.display()
      ),
    )
  };
  #[cfg(unix)]
  {
    use std::os::unix::fs::symlink;
    symlink(&oldpath, &newpath).map_err(err_mapper)?;
  }
  #[cfg(not(unix))]
  {
    use std::os::windows::fs::symlink_dir;
    symlink_dir(&oldpath, &newpath).map_err(err_mapper)?;
  }
  Ok(())
}

fn symlink_file(oldpath: &Path, newpath: &Path) -> Result<(), AnyError> {
  use std::io::Error;
  let err_mapper = |err: Error| {
    Error::new(
      err.kind(),
      format!(
        "{}, symlink '{}' -> '{}'",
        err,
        oldpath.display(),
        newpath.display()
      ),
    )
  };
  #[cfg(unix)]
  {
    use std::os::unix::fs::symlink;
    symlink(&oldpath, &newpath).map_err(err_mapper)?;
  }
  #[cfg(not(unix))]
  {
    use std::os::windows::fs::symlink_file;
    symlink_file(&oldpath, &newpath).map_err(err_mapper)?;
  }
  Ok(())
}
