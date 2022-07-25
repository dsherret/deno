// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use deno_core::error::AnyError;
use deno_core::futures::future::BoxFuture;
use deno_core::futures::FutureExt;

use crate::cache::FastInsecureHasher;

use super::cache::NpmCache;
use super::NpmPackageResolution;

// todo:
// - Need to ensure only one process enters this at a time.

pub async fn setup_node_modules(
  packages: Vec<NpmPackageResolution>,
  cache: NpmCache,
  output_dir: PathBuf,
) -> Result<(), AnyError> {
  fs::create_dir_all(&output_dir)?;
  let resolution_hash_path = output_dir.join(".deno_resolution");
  let resolution_hash = calculate_resolution_hash(&packages);

  if !is_node_modules_up_to_date(&resolution_hash_path, resolution_hash) {
    setup_packages(packages, cache, output_dir).await?;

    // all done, so write the resolution hash
    fs::write(resolution_hash_path, resolution_hash.to_string())?;
  }

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
      let cache_folder =
        cache.ensure_package(&package.id, &package.dist).await?;
      let local_folder = output_dir.join(&package.id.name);
      if package.sub_packages.is_empty() {
        // ensure the local directory doesn't exist
        fs::remove_dir_all(&local_folder)?;
        // no sub packages, so create a symlink
        symlink_dir(&local_folder, &cache_folder)?;
      } else {
        // todo: check if the directoy exists... if it does, then check
        // it's package.json's name and version and skip copying it and
        // all node_modules folders if it's the same

        // there's sub packages, so copy the data here
        copy_dir(&cache_folder, &local_folder)?;
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

fn is_node_modules_up_to_date(
  resolution_hash_path: &Path,
  resolution_hash: u64,
) -> bool {
  match fs::read_to_string(resolution_hash_path) {
    Ok(text) => text.trim() == resolution_hash.to_string(),
    Err(err) => false,
  }
}

fn calculate_resolution_hash(packages: &[NpmPackageResolution]) -> u64 {
  fn with_hasher_for_packages(
    packages: &[NpmPackageResolution],
    hasher: &mut FastInsecureHasher,
  ) {
    // todo(dsherret): debug assert that packages are sorted
    for package in packages {
      hasher.write_str(&package.id.name);
      hasher.write_str(&package.id.version.to_string());
      with_hasher_for_packages(&package.sub_packages, hasher);
    }
  }

  let mut hasher = FastInsecureHasher::new();
  with_hasher_for_packages(packages, &mut hasher);
  hasher.finish()
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), AnyError> {
  debug_assert!(from.is_dir());
  fs::create_dir(&to)?;
  for entry in fs::read_dir(from)? {
    let entry = entry?;
    if entry.file_type()?.is_dir() {
      copy_dir(&entry.path(), &to.join(entry.file_name()))?;
    } else {
      fs::copy(entry.path(), &to.join(entry.file_name()))?;
    }
  }
  Ok(())
}

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
