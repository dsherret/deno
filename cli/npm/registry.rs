// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::collections::HashMap;
use std::sync::Arc;

use deno_core::anyhow::bail;
use deno_core::anyhow::Context;
use deno_core::error::AnyError;
use deno_core::parking_lot::Mutex;
use deno_core::serde::Deserialize;
use deno_core::serde_json;
use deno_core::url::Url;
use deno_runtime::deno_fetch::reqwest;

use super::resolution::NpmPackageReq;

// npm registry docs: https://github.com/npm/registry/blob/master/docs/REGISTRY-API.md

#[derive(Deserialize, Clone)]
pub struct NpmPackageInfo {
  pub name: String,
  pub versions: HashMap<String, NpmPackageVersionInfo>,
}

pub struct NpmDependencyEntry {
  pub bare_specifier: String,
  pub req: NpmPackageReq,
}

#[derive(Deserialize, Clone)]
pub struct NpmPackageVersionInfo {
  pub version: String,
  pub dist: NpmPackageVersionDistInfo,
  // Bare specifier to version (ex. `"typescript": "^3.0.1") or possibly
  // package and version (ex. `"typescript-3.0.1": "npm:typescript@3.0.1"`).
  #[serde(default)]
  pub dependencies: HashMap<String, String>,
}

impl NpmPackageVersionInfo {
  pub fn dependencies_as_references(
    &self,
  ) -> Result<Vec<NpmDependencyEntry>, AnyError> {
    fn entry_as_bare_specifier_and_reference(
      entry: (&String, &String),
    ) -> Result<NpmDependencyEntry, AnyError> {
      let bare_specifier = entry.0.clone();
      let (name, version_req) =
        if let Some(package_and_version) = entry.1.strip_prefix("npm:") {
          if let Some((name, version)) = package_and_version.rsplit_once('@') {
            (name.to_string(), version.to_string())
          } else {
            bail!("could not find @ symbol in npm scheme url '{}'", entry.1);
          }
        } else {
          (entry.0.clone(), entry.1.clone())
        };
      let version_req = semver::VersionReq::parse(&version_req)
        .with_context(|| format!("Dependency: {}", bare_specifier))?;
      Ok(NpmDependencyEntry {
        bare_specifier,
        req: NpmPackageReq { name, version_req },
      })
    }

    self
      .dependencies
      .iter()
      .map(|entry| entry_as_bare_specifier_and_reference(entry))
      .collect::<Result<Vec<_>, AnyError>>()
  }
}

#[derive(Debug, Deserialize, Clone)]
pub struct NpmPackageVersionDistInfo {
  /// URL to the tarball.
  pub tarball: String,
  pub integrity: String,
}

#[derive(Clone)]
pub struct NpmRegistryApi {
  base_url: Url,
  cache: Arc<Mutex<HashMap<String, Option<NpmPackageInfo>>>>,
}

impl Default for NpmRegistryApi {
  fn default() -> Self {
    Self::from_base(Url::parse("https://registry.npmjs.org").unwrap())
  }
}

impl NpmRegistryApi {
  pub fn from_base(base_url: Url) -> Self {
    Self {
      base_url,
      cache: Default::default(),
    }
  }

  pub async fn package_info(
    &self,
    name: &str,
  ) -> Result<NpmPackageInfo, AnyError> {
    let maybe_package_info = self.maybe_package_info(name).await?;
    match maybe_package_info {
      Some(package_info) => Ok(package_info),
      None => bail!("package '{}' does not exist", name),
    }
  }

  pub async fn maybe_package_info(
    &self,
    name: &str,
  ) -> Result<Option<NpmPackageInfo>, AnyError> {
    let maybe_info = self.cache.lock().get(name).cloned();
    if let Some(info) = maybe_info {
      Ok(info)
    } else {
      let maybe_package_info =
        self.maybe_package_info_inner(name).await.with_context(|| {
          format!("Error getting response at {}", self.get_package_url(name))
        })?;
      // Not worth the complexity to ensure multiple in-flight requests
      // for the same package only request once because with how this is
      // used that should never happen. If it does, not a big deal.
      self
        .cache
        .lock()
        .insert(name.to_string(), maybe_package_info.clone());
      Ok(maybe_package_info)
    }
  }

  async fn maybe_package_info_inner(
    &self,
    name: &str,
  ) -> Result<Option<NpmPackageInfo>, AnyError> {
    let response = reqwest::get(self.get_package_url(name)).await?;

    if response.status() == 404 {
      Ok(None)
    } else if !response.status().is_success() {
      bail!("Bad response: {:?}", response.status());
    } else {
      let bytes = response.bytes().await?;
      let package_info = serde_json::from_slice(&bytes)?;
      Ok(Some(package_info))
    }
  }

  fn get_package_url(&self, name: &str) -> Url {
    self.base_url.join(name).unwrap()
  }
}
