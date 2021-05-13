use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Result};
use regex::Regex;
use semver::Version;

use crate::metadata;
use crate::metadata::UnprocessedObj;

fn check_progs(objs: &[UnprocessedObj]) -> Result<()> {
    let mut set = HashSet::with_capacity(objs.len());
    for obj in objs {
        // OK to unwrap() file_name() b/c we already checked earlier that this is a valid file
        let dest = obj
            .out
            .as_path()
            .join(obj.path.as_path().file_name().unwrap());
        if !set.insert(dest) {
            bail!(
                "Duplicate obj={} detected",
                obj.path.as_path().file_name().unwrap().to_string_lossy()
            );
        }
    }

    Ok(())
}

fn extract_version(output: &str) -> Result<&str> {
    let re = Regex::new(r"clang\s+version\s+(?P<version_str>\d+\.\d+\.\d+)")?;
    let captures = re
        .captures(output)
        .ok_or_else(|| anyhow!("Failed to run regex on version string"))?;

    captures.name("version_str").map_or_else(
        || Err(anyhow!("Failed to find version capture group")),
        |v| Ok(v.as_str()),
    )
}

fn check_clang(debug: bool, clang: &Path, skip_version_checks: bool) -> Result<()> {
    let output = Command::new(clang.as_os_str()).arg("--version").output()?;

    if !output.status.success() {
        bail!("Failed to execute clang binary");
    }

    if skip_version_checks {
        return Ok(());
    }

    // Example output:
    //
    //     clang version 10.0.0
    //     Target: x86_64-pc-linux-gnu
    //     Thread model: posix
    //     InstalledDir: /bin
    //
    let output = String::from_utf8_lossy(&output.stdout);
    let version_str = extract_version(&output)?;
    let version = Version::parse(version_str)?;
    if debug {
        println!("{} is version {}", clang.display(), version);
    }

    if version < Version::parse("10.0.0").unwrap() {
        bail!(
            "version {} is too old. Use --skip-clang-version-checks to skip version check",
            version
        );
    }

    Ok(())
}

/// We're essentially going to run:
///
///   clang -g -O2 -target bpf -c -D__TARGET_ARCH_$(ARCH) runqslower.bpf.c -o runqslower.bpf.o
///
/// for each prog.
fn compile_one(debug: bool, source: &Path, out: &Path, clang: &Path, options: &str) -> Result<()> {
    let arch = if std::env::consts::ARCH == "x86_64" {
        "x86"
    } else {
        std::env::consts::ARCH
    };

    if debug {
        println!("Building {}", source.display());
    }

    let mut cmd = Command::new(clang.as_os_str());
    if !options.is_empty() {
        cmd.args(options.split_whitespace());
    }
    cmd.arg("-g")
        .arg("-O2")
        .arg("-target")
        .arg("bpf")
        .arg("-c")
        .arg(format!("-D__TARGET_ARCH_{}", arch))
        .arg(source.as_os_str())
        .arg("-o")
        .arg(out);

    let output = cmd.output()?;
    if !output.status.success() {
        bail!(
            "Failed to compile obj={} with status={}\n \
            stdout=\n \
            {}\n \
            stderr=\n \
            {}\n",
            out.to_string_lossy(),
            output.status,
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap()
        );
    }

    Ok(())
}

fn compile(debug: bool, objs: &[UnprocessedObj], clang: &Path) -> Result<()> {
    for obj in objs {
        let dest_name = if let Some(f) = obj.path.file_stem() {
            let mut stem = f.to_os_string();
            stem.push(".o");
            stem
        } else {
            bail!(
                "Could not calculate destination name for obj={}",
                obj.path.display()
            );
        };
        let mut dest_path = obj.out.to_path_buf();
        dest_path.push(&dest_name);
        fs::create_dir_all(&obj.out)?;
        compile_one(debug, &obj.path, &dest_path, clang, "")?;
    }

    Ok(())
}

pub fn build(
    debug: bool,
    manifest_path: Option<&PathBuf>,
    clang: &Path,
    skip_clang_version_checks: bool,
) -> Result<()> {
    let to_compile = metadata::get(debug, manifest_path)?;

    if debug && !to_compile.is_empty() {
        println!("Found bpf progs to compile:");
        for obj in &to_compile {
            println!("\t{:?}", obj);
        }
    } else if to_compile.is_empty() {
        bail!("Did not find any bpf progs to compile");
    }

    check_progs(&to_compile)?;

    if let Err(e) = check_clang(debug, clang, skip_clang_version_checks) {
        bail!("{} is invalid: {}", clang.display(), e);
    }

    if let Err(e) = compile(debug, &to_compile, clang) {
        bail!("Failed to compile progs: {}", e);
    }

    Ok(())
}

// Only used in libbpf-cargo library
#[allow(dead_code)]
pub fn build_single(
    debug: bool,
    source: &Path,
    out: &Path,
    clang: &Path,
    skip_clang_version_checks: bool,
    options: &str,
) -> Result<()> {
    check_clang(debug, clang, skip_clang_version_checks)?;
    compile_one(debug, source, out, clang, options)?;

    Ok(())
}

#[test]
fn test_extract_version() {
    let upstream_format = r"clang version 10.0.0
Target: x86_64-pc-linux-gnu
Thread model: posix
InstalledDir: /bin
";
    assert_eq!(extract_version(upstream_format).unwrap(), "10.0.0");

    let ubuntu_format = r"Ubuntu clang version 11.0.1-++20201121072624+973b95e0a84-1~exp1~20201121063303.19
Target: x86_64-pc-linux-gnu
Thread model: posix
InstalledDir: /bin
";
    assert_eq!(extract_version(ubuntu_format).unwrap(), "11.0.1");

    assert!(extract_version("askldfjwe").is_err());
    assert!(extract_version("my clang version 1.5").is_err());
}
