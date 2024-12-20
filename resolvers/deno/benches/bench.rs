use std::borrow::Cow;
use std::hint::black_box;
use std::sync::Arc;

use deno_package_json::fs::RealDenoPkgJsonFs;
use deno_path_util::strip_unc_prefix;
use deno_resolver::fs::DenoResolverFs;
use deno_resolver::npm::ByonmInNpmPackageChecker;
use deno_resolver::npm::ByonmNpmResolver;
use deno_resolver::npm::ByonmNpmResolverCreateOptions;
use node_resolver::env::NodeResolverEnv;
use node_resolver::NodeResolver;
use node_resolver::PackageJsonResolver;
use url::Url;

static BUILTIN_NODE_MODULES: &[&str] = &[
  "_http_agent",
  "_http_common",
  "_http_outgoing",
  "_http_server",
  "_stream_duplex",
  "_stream_passthrough",
  "_stream_readable",
  "_stream_transform",
  "_stream_writable",
  "_tls_common",
  "_tls_wrap",
  "assert",
  "assert/strict",
  "async_hooks",
  "buffer",
  "child_process",
  "cluster",
  "console",
  "constants",
  "crypto",
  "dgram",
  "diagnostics_channel",
  "dns",
  "dns/promises",
  "domain",
  "events",
  "fs",
  "fs/promises",
  "http",
  "http2",
  "https",
  "inspector",
  "module",
  "net",
  "os",
  "path",
  "path/posix",
  "path/win32",
  "perf_hooks",
  "process",
  "punycode",
  "querystring",
  "readline",
  "readline/promises",
  "repl",
  "stream",
  "stream/consumers",
  "stream/promises",
  "stream/web",
  "string_decoder",
  "sys",
  "test",
  "timers",
  "timers/promises",
  "tls",
  "tty",
  "url",
  "util",
  "util/types",
  "v8",
  "vm",
  "wasi",
  "worker_threads",
  "zlib",
];

#[derive(Debug)]
struct RealEnv;

impl NodeResolverEnv for RealEnv {
  fn is_builtin_node_module(&self, specifier: &str) -> bool {
    BUILTIN_NODE_MODULES.iter().any(|s| *s == specifier)
  }

  fn realpath_sync(
    &self,
    path: &std::path::Path,
  ) -> std::io::Result<std::path::PathBuf> {
    Ok(strip_unc_prefix(path.canonicalize()?))
  }

  fn stat_sync(
    &self,
    path: &std::path::Path,
  ) -> std::io::Result<node_resolver::env::NodeResolverFsStat> {
    path
      .metadata()
      .map(|metadata| node_resolver::env::NodeResolverFsStat {
        is_file: metadata.is_file(),
        is_dir: metadata.is_dir(),
      })
  }

  fn exists_sync(&self, path: &std::path::Path) -> bool {
    path.exists()
  }

  fn pkg_json_fs(&self) -> &dyn deno_package_json::fs::DenoPkgJsonFs {
    &RealDenoPkgJsonFs
  }
}

#[derive(Debug)]
struct RealDenoResolverFs;

impl DenoResolverFs for RealDenoResolverFs {
  fn read_to_string_lossy(
    &self,
    path: &std::path::Path,
  ) -> std::io::Result<Cow<'static, str>> {
    std::fs::read_to_string(path).map(Cow::Owned)
  }

  fn realpath_sync(
    &self,
    path: &std::path::Path,
  ) -> std::io::Result<std::path::PathBuf> {
    Ok(strip_unc_prefix(path.canonicalize()?))
  }

  fn exists_sync(&self, path: &std::path::Path) -> bool {
    path.exists()
  }

  fn is_dir_sync(&self, path: &std::path::Path) -> bool {
    path.is_dir()
  }

  fn read_dir_sync(
    &self,
    dir_path: &std::path::Path,
  ) -> std::io::Result<Vec<deno_resolver::fs::DirEntry>> {
    let entries = std::fs::read_dir(dir_path)?
      .map(|entry| {
        let entry = entry?;
        let metadata = entry.metadata()?;
        Ok(deno_resolver::fs::DirEntry {
          name: entry.file_name().into_string().unwrap(),
          is_file: metadata.is_file(),
          is_directory: metadata.is_dir(),
        })
      })
      .collect::<std::io::Result<Vec<deno_resolver::fs::DirEntry>>>();
    entries
  }
}

fn main() {
  divan::main();
}

#[divan::bench(sample_count = 1000)]
fn resolution(bencher: divan::Bencher) {
  let pkg_json_resolver = Arc::new(PackageJsonResolver::new(RealEnv));
  let resolver = NodeResolver::new(
    RealEnv,
    Arc::new(ByonmInNpmPackageChecker),
    Arc::new(ByonmNpmResolver::new(ByonmNpmResolverCreateOptions {
      root_node_modules_dir: None,
      fs: RealDenoResolverFs,
      pkg_json_resolver: pkg_json_resolver.clone(),
    })),
    pkg_json_resolver,
  );

  let url = Url::parse(
    "file:///V:/parcel/packages/utils/node-resolver-core/test/fixture/root.js",
  )
  .unwrap();

  bencher.bench_local(|| {
    node_resolver::PackageJsonThreadLocalCache::clear();

    for specifier in &[
      "./nested/index.js",
      "@parcel/core",
      "axios",
      "@babel/parser",
    ] {
      let _ = black_box(resolver.resolve(
        black_box(specifier),
        &url,
        node_resolver::ResolutionMode::Import,
        node_resolver::NodeResolutionKind::Execution,
      ))
      .unwrap();
    }
  });
}
