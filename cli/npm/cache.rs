// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::fs;
use std::path::PathBuf;

use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_runtime::colors;
use deno_runtime::deno_fetch::reqwest;

use super::tarball::verify_and_extract_tarball;
use super::NpmPackageId;
use super::NpmPackageVersionDistInfo;

/// Stores a single copy of npm packages in a cache.
#[derive(Clone)]
pub struct NpmCache {
  location: PathBuf,
}

impl NpmCache {
  pub fn new(location: PathBuf) -> Self {
    Self { location }
  }

  pub async fn ensure_package(
    &self,
    id: &NpmPackageId,
    dist: &NpmPackageVersionDistInfo,
  ) -> Result<PathBuf, AnyError> {
    let package_folder = self.package_folder(id);
    if package_folder.exists() {
      return Ok(package_folder);
    }

    log::log!(
      log::Level::Info,
      "{} {}",
      colors::green("Download"),
      dist.tarball,
    );

    let response = reqwest::get(&dist.tarball).await?;

    if response.status() == 404 {
      bail!("Could not find npm package tarball at: {}", dist.tarball);
    } else if !response.status().is_success() {
      bail!("Bad response: {:?}", response.status());
    } else {
      let bytes = response.bytes().await?;

      match verify_and_extract_tarball(
        id,
        &bytes,
        &dist.integrity,
        &package_folder,
      ) {
        Ok(()) => Ok(package_folder),
        Err(err) => {
          if let Err(remove_err) = fs::remove_dir_all(&package_folder) {
            if remove_err.kind() != std::io::ErrorKind::NotFound {
              bail!(
                concat!(
                  "Failed verifying and extracting npm tarball for {}, then ",
                  "failed cleaning up package cache folder.\n\nOriginal ",
                  "error:\n\n{}\n\nRemove error:\n\n{}\n\nPlease manually ",
                  "delete this folder or you will run into issues using this ",
                  "package in the future:\n\n{}"
                ),
                id,
                err,
                remove_err,
                package_folder.display(),
              );
            }
          }
          Err(err)
        }
      }
    }
  }

  pub fn package_folder(&self, id: &NpmPackageId) -> PathBuf {
    self
      .location
      .join(&id.name)
      .join(id.version.to_string())
      .join("package")
  }
}
