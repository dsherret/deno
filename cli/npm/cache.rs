// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::fs;
use std::path::PathBuf;

use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_runtime::colors;
use deno_runtime::deno_fetch::reqwest;

use super::tarball::extract_tarball;
use super::tarball::verify_tarball;
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

    fs::create_dir_all(&package_folder)?;

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
      verify_tarball(id, &bytes, &dist.shasum)?;
      extract_tarball(&bytes, &package_folder)?;
      Ok(package_folder)
    }
  }

  fn package_folder(&self, id: &NpmPackageId) -> PathBuf {
    self.location.join(&id.name).join(id.version.to_string())
  }
}
