// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::path::Path;

use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use flate2::read::GzDecoder;
use tar::Archive;

use crate::checksum;

use super::NpmPackageId;

pub fn verify_tarball(
  package: &NpmPackageId,
  data: &[u8],
  expected_shasum: &str,
) -> Result<(), AnyError> {
  let expected_shasum = expected_shasum.to_lowercase();
  let tarball_checksum = checksum::gen(&[data]).to_lowercase();
  if tarball_checksum != expected_shasum {
    bail!(
      "Checksum did not match for {}.\n\nExpected: {}\nActual: {}",
      package,
      expected_shasum,
      tarball_checksum,
    )
  }
  Ok(())
}

pub fn extract_tarball(
  data: &[u8],
  output_folder: &Path,
) -> Result<(), AnyError> {
  let tar = GzDecoder::new(data);
  let mut archive = Archive::new(tar);
  archive.unpack(output_folder)?;

  Ok(())
}

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  pub fn test_verify_tarball() {
    // this needs to be sha256
    let package_id = NpmPackageId {
      name: "package".to_string(),
      version: semver::Version::parse("1.0.0").unwrap(),
    };
    let actual_checksum =
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert!(verify_tarball(&package_id, &Vec::new(), &actual_checksum).is_ok());
    assert_eq!(
      verify_tarball(&package_id, &Vec::new(), "test")
        .unwrap_err()
        .to_string(),
      format!("Checksum did not match for package@1.0.0.\n\nExpected: test\nActual: {}", actual_checksum),
    );
  }
}
