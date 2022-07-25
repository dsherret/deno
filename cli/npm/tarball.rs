// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::path::Path;

use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use flate2::read::GzDecoder;
use tar::Archive;

use super::NpmPackageId;

pub fn verify_and_extract_tarball(
  package: &NpmPackageId,
  data: &[u8],
  npm_integrity: &str,
  output_folder: &Path,
) -> Result<(), AnyError> {
  verify_tarball(package, data, npm_integrity)?;
  extract_tarball(data, output_folder)
}

fn verify_tarball(
  package: &NpmPackageId,
  data: &[u8],
  npm_integrity: &str,
) -> Result<(), AnyError> {
  use ring::digest::Context;
  use ring::digest::SHA512;
  let (algo, expected_checksum) = match npm_integrity.split_once('-') {
    Some((hash_kind, checksum)) => {
      let algo = match hash_kind {
        "sha512" => &SHA512,
        hash_kind => bail!(
          "not implemented hash function for {}: {}",
          package,
          hash_kind
        ),
      };
      (algo, checksum.to_lowercase())
    }
    None => bail!(
      "not implemented integrity kind for {}: {}",
      package,
      npm_integrity
    ),
  };

  let mut hash_ctx = Context::new(algo);
  hash_ctx.update(data);
  let digest = hash_ctx.finish();
  let tarball_checksum = base64::encode(digest.as_ref()).to_lowercase();
  if tarball_checksum != expected_checksum {
    bail!(
      "tarball checksum did not match what was provided by npm registry for {}.\n\nExpected: {}\nActual: {}",
      package,
      expected_checksum,
      tarball_checksum,
    )
  }
  Ok(())
}

fn extract_tarball(data: &[u8], output_folder: &Path) -> Result<(), AnyError> {
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
    let package_id = NpmPackageId {
      name: "package".to_string(),
      version: semver::Version::parse("1.0.0").unwrap(),
    };
    let actual_checksum =
      "z4phnx7vul3xvchq1m2ab9yg5aulvxxcg/spidns6c5h0ne8xyxysp+dgnkhfuwvy7kxvudbeoglodj6+sfapg==";
    assert_eq!(
      verify_tarball(&package_id, &Vec::new(), "test")
        .unwrap_err()
        .to_string(),
      "not implemented integrity kind for package@1.0.0: test",
    );
    assert_eq!(
      verify_tarball(&package_id, &Vec::new(), "sha1-test")
        .unwrap_err()
        .to_string(),
      "not implemented hash function for package@1.0.0: sha1",
    );
    assert_eq!(
      verify_tarball(&package_id, &Vec::new(), "sha512-test")
        .unwrap_err()
        .to_string(),
      format!("tarball checksum did not match what was provided by npm registry for package@1.0.0.\n\nExpected: test\nActual: {}", actual_checksum),
    );
    assert!(verify_tarball(
      &package_id,
      &Vec::new(),
      &format!("sha512-{}", actual_checksum)
    )
    .is_ok());
  }
}
