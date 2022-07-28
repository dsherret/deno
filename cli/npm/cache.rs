// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::fs;
use std::path::PathBuf;

use deno_ast::ModuleSpecifier;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::url::Url;
use deno_runtime::colors;
use deno_runtime::deno_fetch::reqwest;

use super::tarball::verify_and_extract_tarball;
use super::NpmPackageId;
use super::NpmPackageVersionDistInfo;

/// Stores a single copy of npm packages in a cache.
#[derive(Clone)]
pub struct NpmCache {
  root_dir: PathBuf,
  // cached url representation of the root directory
  root_dir_url: Url,
}

impl NpmCache {
  pub fn new(root_dir: PathBuf) -> Self {
    let root_dir_url = Url::from_directory_path(&root_dir).unwrap();
    Self {
      root_dir,
      root_dir_url,
    }
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
    let name_parts = id.name.split('/');
    let mut dir = self.root_dir.clone();
    // ensure backslashes are used on windows
    for part in name_parts {
      dir = dir.join(part);
    }
    dir.join(id.version.to_string()).join("package")
  }

  pub fn get_package_from_referrer(
    &self,
    referrer: &ModuleSpecifier,
  ) -> Option<NpmPackageId> {
    let relative_url = self.root_dir_url.make_relative(referrer)?;
    if relative_url.starts_with("../") {
      return None;
    }

    // examples:
    // * chalk/5.0.1/package
    // * @types/chalk/5.0.1/package
    let mut parts = relative_url
      .split('/')
      .enumerate()
      .take_while(|(i, part)| *i < 2 || *part != "package")
      .map(|(_, part)| part)
      .collect::<Vec<_>>();
    let version = parts.pop().unwrap();
    let name = parts.join("/");

    Some(NpmPackageId {
      name,
      version: semver::Version::parse(version).unwrap(),
    })
  }
}
